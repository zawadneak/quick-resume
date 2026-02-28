/// Restore saved thread register contexts into a newly launched process.
///
/// Phase 3e: Match saved thread contexts to the new process's threads by
/// position (index), then call SetThreadContext for each.
///
/// Note: thread IDs will be different in the new process. We match them
/// by creation order, which is consistent for deterministic Unity startup.
use std::mem;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::{SetThreadContext, CONTEXT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Threading::{OpenThread, THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME};

use crate::snapshot::threads::ThreadSnapshot;
use crate::util::error::{QuickResumeError, Result};

/// Restore thread contexts from `snapshots` into the process with `pid`.
///
/// The new process must be fully suspended before this is called.
pub fn restore_thread_contexts(pid: u32, snapshots: &[ThreadSnapshot]) -> Result<()> {
    let new_tids = collect_tids(pid)?;

    if new_tids.is_empty() {
        return Err(QuickResumeError::Other(
            "No threads found in restored process".into(),
        ));
    }

    let pair_count = new_tids.len().min(snapshots.len());
    let mut failed = 0usize;

    for i in 0..pair_count {
        let new_tid = new_tids[i];
        let snap = &snapshots[i];

        if let Err(e) = restore_one(new_tid, snap) {
            eprintln!(
                "[restore::threads] TID {} (slot {}): {}",
                new_tid, i, e
            );
            failed += 1;
        } else {
            println!(
                "[restore::threads] Restored context: saved TID {} → new TID {}",
                snap.tid, new_tid
            );
        }
    }

    if snapshots.len() > new_tids.len() {
        eprintln!(
            "[restore::threads] Warning: snapshot has {} threads, new process has {} — {} context(s) dropped",
            snapshots.len(),
            new_tids.len(),
            snapshots.len() - new_tids.len()
        );
    }

    if failed > 0 {
        return Err(QuickResumeError::Other(format!(
            "{} thread context restores failed",
            failed
        )));
    }

    Ok(())
}

fn restore_one(tid: u32, snap: &ThreadSnapshot) -> Result<()> {
    if snap.context_bytes.len() != mem::size_of::<CONTEXT>() {
        return Err(QuickResumeError::Other(format!(
            "TID {}: context bytes size mismatch ({} vs {})",
            tid,
            snap.context_bytes.len(),
            mem::size_of::<CONTEXT>()
        )));
    }

    let thread_handle =
        unsafe { OpenThread(THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME, false, tid) }
            .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    // Reconstruct the CONTEXT from raw bytes.
    let mut ctx = AlignedContext::new();
    unsafe {
        std::ptr::copy_nonoverlapping(
            snap.context_bytes.as_ptr(),
            &mut ctx.inner as *mut CONTEXT as *mut u8,
            mem::size_of::<CONTEXT>(),
        );
    }

    let result = unsafe { SetThreadContext(thread_handle, &ctx.inner) };
    unsafe { let _ = CloseHandle(thread_handle); }

    result.map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;
    Ok(())
}

fn collect_tids(pid: u32) -> Result<Vec<u32>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?;
    let mut tids = Vec::new();
    let mut entry = THREADENTRY32 {
        dwSize: mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe {
        if Thread32First(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32OwnerProcessID == pid {
                    tids.push(entry.th32ThreadID);
                }
                if Thread32Next(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(tids)
}

#[repr(align(16))]
struct AlignedContext {
    inner: CONTEXT,
}

impl AlignedContext {
    fn new() -> Self {
        AlignedContext {
            inner: unsafe { mem::zeroed() },
        }
    }
}
