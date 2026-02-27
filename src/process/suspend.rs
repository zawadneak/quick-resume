/// RAII guard for NtSuspendProcess / NtResumeProcess.
///
/// Suspending via NtSuspendProcess atomically freezes every thread in the
/// target process. The guard resumes the process on drop so that a panic or
/// early return never leaves the target permanently frozen.
use windows::Win32::Foundation::HANDLE;
use windows::Wdk::System::Threading::{NtResumeProcess, NtSuspendProcess};

use crate::util::error::{QuickResumeError, Result};

pub struct SuspendGuard {
    handle: HANDLE,
    resumed: bool,
}

impl SuspendGuard {
    /// Suspend the process referenced by `handle` and return the guard.
    pub fn suspend(handle: HANDLE) -> Result<Self> {
        let status = unsafe { NtSuspendProcess(handle) };
        if status.is_err() {
            return Err(QuickResumeError::Other(format!(
                "NtSuspendProcess failed: NTSTATUS={:#010X}",
                status.0
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
            let status = unsafe { NtResumeProcess(self.handle) };
            if status.is_err() {
                return Err(QuickResumeError::Other(format!(
                    "NtResumeProcess failed: NTSTATUS={:#010X}",
                    status.0
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
            // Best-effort resume — ignore errors on drop path
            unsafe {
                let _ = NtResumeProcess(self.handle);
            }
        }
    }
}
