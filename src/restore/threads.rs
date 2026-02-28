/// Restore saved thread register contexts into a newly launched process.
///
/// Phase 3e: Match saved thread contexts to the new process's threads by
/// position (index), then call SetThreadContext / Wow64SetThreadContext for each.
///
/// For 32-bit (WOW64) processes, Wow64SetThreadContext is used — symmetric
/// with how Wow64GetThreadContext was used during snapshot capture.
use std::mem;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::{
    SetThreadContext, Wow64SetThreadContext, CONTEXT, WOW64_CONTEXT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Threading::{OpenThread, THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME};

use crate::snapshot::threads::ThreadSnapshot;
use crate::util::error::{QuickResumeError, Result};

/// Restore thread contexts from `snapshots` into the process with `pid`.
///
/// Pass `is_wow64 = true` for 32-bit (WOW64) games — must match what was used
/// during snapshot capture so the context byte sizes are consistent.
///
/// The new process must be fully suspended before this is called.
pub fn restore_thread_contexts(
    pid: u32,
    snapshots: &[ThreadSnapshot],
    is_wow64: bool,
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

        if let Err(e) = restore_one(new_tid, snap, is_wow64) {
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

fn restore_one(tid: u32, snap: &ThreadSnapshot, is_wow64: bool) -> Result<()> {
    let expected_size = if is_wow64 {
        mem::size_of::<WOW64_CONTEXT>()
    } else {
        mem::size_of::<CONTEXT>()
    };

    if snap.context_bytes.len() != expected_size {
        return Err(QuickResumeError::Other(format!(
            "TID {}: context bytes size mismatch ({} vs {})",
            tid,
            snap.context_bytes.len(),
            expected_size
        )));
    }

    let thread_handle =
        unsafe { OpenThread(THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME, false, tid) }
            .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let result = if is_wow64 {
        restore_wow64(tid, thread_handle, snap)
    } else {
        restore_x64(tid, thread_handle, snap)
    };

    unsafe { let _ = CloseHandle(thread_handle); }
    result
}

fn restore_x64(
    tid: u32,
    thread_handle: windows::Win32::Foundation::HANDLE,
    snap: &ThreadSnapshot,
) -> Result<()> {
    let mut ctx = AlignedX64Context::new();
    unsafe {
        std::ptr::copy_nonoverlapping(
            snap.context_bytes.as_ptr(),
            &mut ctx.inner as *mut CONTEXT as *mut u8,
            mem::size_of::<CONTEXT>(),
        );
    }
    unsafe { SetThreadContext(thread_handle, &ctx.inner) }
        .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })
}

fn restore_wow64(
    tid: u32,
    thread_handle: windows::Win32::Foundation::HANDLE,
    snap: &ThreadSnapshot,
) -> Result<()> {
    let mut ctx: WOW64_CONTEXT = unsafe { mem::zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(
            snap.context_bytes.as_ptr(),
            &mut ctx as *mut WOW64_CONTEXT as *mut u8,
            mem::size_of::<WOW64_CONTEXT>(),
        );
    }
    unsafe { Wow64SetThreadContext(thread_handle, &ctx) }
        .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })
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
