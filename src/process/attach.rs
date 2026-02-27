/// Locate a running process by executable name and open a handle to it.
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

use crate::util::error::{QuickResumeError, Result};

/// A wrapper around a Win32 process HANDLE that closes on drop.
pub struct ProcessHandle {
    pub handle: HANDLE,
    pub pid: u32,
    pub name: String,
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Find the first process whose executable name matches `target_name`
/// (case-insensitive, e.g. "Mini Metro.exe") and open it with PROCESS_ALL_ACCESS.
pub fn open_process_by_name(target_name: &str) -> Result<ProcessHandle> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }?;

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut found_pid: Option<u32> = None;
    let mut found_name = String::new();

    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if name.eq_ignore_ascii_case(target_name) {
                    found_pid = Some(entry.th32ProcessID);
                    found_name = name;
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }

    let pid = found_pid.ok_or_else(|| QuickResumeError::ProcessNotFound(target_name.to_string()))?;

    let handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, pid) }
        .map_err(|e| QuickResumeError::OpenProcessFailed { pid, source: e })?;

    Ok(ProcessHandle {
        handle,
        pid,
        name: found_name,
    })
}

/// Return the PIDs of all running processes matching `target_name`.
pub fn find_all_pids(target_name: &str) -> Result<Vec<u32>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }?;

    let mut pids = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if name.eq_ignore_ascii_case(target_name) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }

    Ok(pids)
}
