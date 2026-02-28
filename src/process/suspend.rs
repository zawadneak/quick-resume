/// RAII guard for NtSuspendProcess / NtResumeProcess.
///
/// These are undocumented NT APIs exported by ntdll.dll. We declare them
/// with raw `extern "system"` FFI so both the MSVC and GNU toolchains can
/// link them without needing the WDK headers.
///
/// Suspending via NtSuspendProcess atomically freezes every thread in the
/// target process. The guard resumes on drop so a panic or early return never
/// leaves the target permanently frozen.
use windows::Win32::Foundation::HANDLE;

use crate::util::error::{QuickResumeError, Result};

extern "system" {
    fn NtSuspendProcess(process_handle: HANDLE) -> i32;
    fn NtResumeProcess(process_handle: HANDLE) -> i32;
}

// Ensure the linker pulls in ntdll.lib / libntdll.a.
#[link(name = "ntdll")]
extern "system" {}

pub struct SuspendGuard {
    handle: HANDLE,
    resumed: bool,
}

impl SuspendGuard {
    /// Suspend the process referenced by `handle` and return the guard.
    pub fn suspend(handle: HANDLE) -> Result<Self> {
        // SAFETY: handle is a valid process handle opened with PROCESS_ALL_ACCESS.
        let status = unsafe { NtSuspendProcess(handle) };
        if status < 0 {
            return Err(QuickResumeError::Other(format!(
                "NtSuspendProcess failed: NTSTATUS={:#010X}",
                status as u32
            )));
        }
        Ok(SuspendGuard {
            handle,
            resumed: false,
        })
    }

    /// Resume the process manually before the guard drops.
    pub fn resume(&mut self) -> Result<()> {
        if !self.resumed {
            // SAFETY: same handle, still valid.
            let status = unsafe { NtResumeProcess(self.handle) };
            if status < 0 {
                return Err(QuickResumeError::Other(format!(
                    "NtResumeProcess failed: NTSTATUS={:#010X}",
                    status as u32
                )));
            }
            self.resumed = true;
        }
        Ok(())
    }
}

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        if !self.resumed {
            // Best-effort resume — ignore errors on the drop path.
            unsafe {
                NtResumeProcess(self.handle);
            }
        }
    }
}
