/// Launch the target executable in a suspended state ready for memory injection.
///
/// Phase 3b / 3c of the restore plan:
///   1. Spawn the process with CREATE_SUSPENDED so it doesn't execute user code.
///   2. Wait until ntdll + kernel32 are mapped (the loader has initialized).
///   3. Return the process and thread handles for further patching.
use std::ffi::OsStr;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W,
    TH32CS_SNAPMODULE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, ResumeThread, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::util::error::{QuickResumeError, Result};

/// Handles returned for the newly launched suspended process.
pub struct SuspendedProcess {
    pub process_handle: HANDLE,
    pub main_thread_handle: HANDLE,
    pub pid: u32,
    pub tid: u32,
}

impl SuspendedProcess {
    /// Resume the main thread (and all process threads resume naturally).
    pub fn resume_main_thread(&self) -> Result<()> {
        let prev = unsafe { ResumeThread(self.main_thread_handle) };
        if prev == u32::MAX {
            return Err(QuickResumeError::Other("ResumeThread failed".into()));
        }
        Ok(())
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process_handle);
            let _ = CloseHandle(self.main_thread_handle);
        }
    }
}

/// Spawn `exe_path` with CREATE_SUSPENDED and wait until the OS loader has
/// mapped ntdll and kernel32 (i.e., the CRT is ready for us to patch memory).
pub fn launch_suspended(exe_path: &Path) -> Result<SuspendedProcess> {
    let wide_path = to_wide(exe_path.to_str().unwrap_or(""));

    let mut si = STARTUPINFOW {
        cb: mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    // CREATE_SUSPENDED (0x00000004)
    const CREATE_SUSPENDED: u32 = 0x0000_0004;

    unsafe {
        CreateProcessW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            // lpCommandLine: PWSTR — pass null, we use lpApplicationName instead
            windows::core::PWSTR(std::ptr::null_mut()),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(CREATE_SUSPENDED),
            None,
            None,
            &mut si,
            &mut pi,
        )?;
    }

    let proc = SuspendedProcess {
        process_handle: pi.hProcess,
        main_thread_handle: pi.hThread,
        pid: pi.dwProcessId,
        tid: pi.dwThreadId,
    };

    println!(
        "[launch] Spawned PID {} (main TID {}) in suspended state.",
        proc.pid, proc.tid
    );

    // Wait for the loader to map system DLLs.
    wait_for_loader(&proc)?;

    Ok(proc)
}

/// Poll until ntdll.dll and kernel32.dll appear in the module list.
/// This means the OS loader has finished its work and the heap is live.
fn wait_for_loader(proc: &SuspendedProcess) -> Result<()> {
    // Strategy: resume 100 ms → suspend → check modules → repeat up to 60× (6 s).
    // We always suspend before checking so the thread never runs freely on a
    // failed check — otherwise the process would run unconstrained for 6 seconds.
    use windows::Win32::System::Threading::SuspendThread;
    for attempt in 0..60 {
        unsafe { ResumeThread(proc.main_thread_handle) };
        thread::sleep(Duration::from_millis(100));
        unsafe { SuspendThread(proc.main_thread_handle) }; // always re-suspend first

        if loader_ready(proc.pid) {
            println!("[launch] Loader ready after {}×100 ms.", attempt + 1);
            return Ok(());
        }
    }

    Err(QuickResumeError::Other(
        "Loader did not map system DLLs within 6 seconds".into(),
    ))
}

/// Return true once the OS loader has initialised enough for memory injection.
///
/// Uses TH32CS_SNAPMODULE (64-bit modules only) — no SNAPMODULE32 needed:
///
///   Native x64 : ntdll.dll + kernel32.dll  — both 64-bit, always visible.
///   WOW64      : kernel32.dll is 32-bit (SysWOW64) and invisible without
///                SNAPMODULE32, but wow64cpu.dll is a 64-bit module loaded by
///                ntdll before any 32-bit code runs — use that instead.
fn loader_ready(pid: u32) -> bool {
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid) }) else {
        return false;
    };

    let mut entry = MODULEENTRY32W {
        dwSize: mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };

    let mut has_ntdll = false;
    let mut has_kernel32 = false;
    let mut has_wow64cpu = false;

    unsafe {
        if Module32FirstW(snap, &mut entry).is_ok() {
            loop {
                let name = wide_to_string(&entry.szModule);
                if name.eq_ignore_ascii_case("ntdll.dll") {
                    has_ntdll = true;
                }
                if name.eq_ignore_ascii_case("kernel32.dll") {
                    has_kernel32 = true;
                }
                if name.eq_ignore_ascii_case("wow64cpu.dll") {
                    has_wow64cpu = true;
                }
                if has_ntdll && (has_kernel32 || has_wow64cpu) {
                    break;
                }
                if Module32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }

    has_ntdll && (has_kernel32 || has_wow64cpu)
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
