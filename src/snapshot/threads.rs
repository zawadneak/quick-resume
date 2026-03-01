/// Capture the full register context of every thread in a process.
///
/// The process MUST be suspended before calling these functions, otherwise
/// the captured contexts will be inconsistent.
///
/// Always uses 64-bit GetThreadContext — even for WOW64 processes. In WOW64,
/// all threads actually execute in 64-bit mode through wow64cpu.dll, so the
/// 64-bit context captures the REAL execution state (including the wow64
/// translation layer position). Wow64GetThreadContext only returns the
/// "emulated" 32-bit registers, which is incomplete for full process restore.
use std::mem;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::{GetThreadContext, CONTEXT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Threading::{OpenThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME};

use crate::util::error::{QuickResumeError, Result};

// CONTEXT_ALL for x64: captures control, integer, floating-point, debug,
// segment, and extended registers.
const CONTEXT_ALL_X64: u32 = 0x0010_003F;

/// Saved register state for a single thread.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ThreadSnapshot {
    pub tid: u32,
    /// Raw bytes of the 64-bit CONTEXT structure.
    pub context_bytes: Vec<u8>,
}

/// Capture the 64-bit context of every thread belonging to `pid`.
///
/// The `_is_wow64` parameter is accepted for API compatibility but ignored —
/// we always use GetThreadContext (64-bit) regardless of process bitness.
pub fn capture_thread_contexts(pid: u32, _is_wow64: bool) -> Result<Vec<ThreadSnapshot>> {
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
    let thread_handle =
        unsafe { OpenThread(THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME, false, tid) }
            .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let mut ctx = AlignedX64Context::new();
    ctx.inner.ContextFlags =
        windows::Win32::System::Diagnostics::Debug::CONTEXT_FLAGS(CONTEXT_ALL_X64);

    let result = unsafe { GetThreadContext(thread_handle, &mut ctx.inner) };
    unsafe { let _ = CloseHandle(thread_handle); }

    result.map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &ctx.inner as *const CONTEXT as *const u8,
            mem::size_of::<CONTEXT>(),
        )
        .to_vec()
    };
    Ok(ThreadSnapshot { tid, context_bytes: bytes })
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

/// 16-byte-aligned wrapper for the x64 CONTEXT (required by GetThreadContext).
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
