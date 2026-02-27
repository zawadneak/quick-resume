/// Capture the full register context of every thread in a process.
///
/// The process MUST be suspended before calling these functions, otherwise
/// the captured contexts will be inconsistent.
use std::mem;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::Debug::{GetThreadContext, CONTEXT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Threading::{OpenThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME};

use crate::util::error::{QuickResumeError, Result};

// CONTEXT_ALL captures every register group on x64.
const CONTEXT_ALL: u32 = 0x0010_003F;

/// Saved register state for a single thread.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ThreadSnapshot {
    pub tid: u32,
    /// Raw bytes of the x64 CONTEXT structure.
    /// Stored as bytes to avoid serde issues with the large union type.
    pub context_bytes: Vec<u8>,
}

/// Capture the CONTEXT of every thread belonging to `pid`.
pub fn capture_thread_contexts(pid: u32) -> Result<Vec<ThreadSnapshot>> {
    let tids = collect_tids(pid)?;
    let mut snapshots = Vec::with_capacity(tids.len());

    for tid in tids {
        match capture_one(tid) {
            Ok(snap) => snapshots.push(snap),
            Err(e) => {
                eprintln!("[threads] TID {}: capture failed — {}", tid, e);
            }
        }
    }

    Ok(snapshots)
}

fn capture_one(tid: u32) -> Result<ThreadSnapshot> {
    let thread_handle = unsafe {
        OpenThread(THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME, false, tid)
    }
    .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    // Allocate a properly aligned CONTEXT.
    // CONTEXT requires 16-byte alignment on x64.
    let mut ctx = AlignedContext::new();
    ctx.inner.ContextFlags = windows::Win32::System::Diagnostics::Debug::CONTEXT_FLAGS(CONTEXT_ALL);

    let result = unsafe { GetThreadContext(thread_handle, &mut ctx.inner) };

    unsafe { let _ = CloseHandle(thread_handle); }

    result.map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    // Serialize the CONTEXT as raw bytes.
    let context_bytes = unsafe {
        std::slice::from_raw_parts(
            &ctx.inner as *const CONTEXT as *const u8,
            mem::size_of::<CONTEXT>(),
        )
        .to_vec()
    };

    Ok(ThreadSnapshot { tid, context_bytes })
}

/// List all thread IDs that belong to `pid`.
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

/// 16-byte-aligned wrapper for CONTEXT (required by GetThreadContext).
#[repr(align(16))]
struct AlignedContext {
    inner: CONTEXT,
}

impl AlignedContext {
    fn new() -> Self {
        // SAFETY: CONTEXT is a plain C struct; zero-init is safe before
        // ContextFlags is set.
        AlignedContext {
            inner: unsafe { mem::zeroed() },
        }
    }
}
