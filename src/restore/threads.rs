/// Restore saved thread register contexts into a newly launched process.
///
/// Always uses 64-bit SetThreadContext — even for WOW64 processes.
/// In WOW64, threads execute in 64-bit mode through wow64cpu.dll, so the
/// 64-bit context is the real execution state. Setting this context correctly
/// positions the thread in the wow64 translation layer, which then resumes
/// 32-bit execution.
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
/// The `_is_wow64` parameter is accepted for API compatibility but ignored —
/// we always use SetThreadContext (64-bit) regardless of process bitness.
///
/// The new process must be fully suspended before this is called.
pub fn restore_thread_contexts(
    pid: u32,
    snapshots: &[ThreadSnapshot],
    _is_wow64: bool,
) -> Result<()> {
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

    let succeeded = pair_count - failed;
    println!(
        "[restore::threads] {}/{} thread contexts restored ({} failed — likely OS-managed threads)",
        succeeded, pair_count, failed
    );

    if succeeded == 0 {
        return Err(QuickResumeError::Other(
            "All thread context restores failed".into(),
        ));
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

    // Reconstruct the 64-bit CONTEXT from raw bytes.
    let mut ctx = AlignedX64Context::new();
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

/// 16-byte-aligned wrapper for the x64 CONTEXT (required by SetThreadContext).
#[repr(align(16))]
struct AlignedX64Context {
    inner: CONTEXT,
}

impl AlignedX64Context {
    fn new() -> Self {
        AlignedX64Context {
            inner: unsafe { mem::zeroed() },
        }
    }
}
