/// Capture the full register context of every thread in a process.
///
/// The process MUST be suspended before calling these functions, otherwise
/// the captured contexts will be inconsistent.
///
/// For 32-bit (WOW64) processes, Wow64GetThreadContext is used — this gives
/// the actual user-mode x86 registers (EIP, ESP, etc.) rather than the x64
/// kernel context that GetThreadContext returns for WOW64 threads.
use std::mem;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::{
    GetThreadContext, Wow64GetThreadContext, CONTEXT, WOW64_CONTEXT, WOW64_CONTEXT_FLAGS,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Threading::{OpenThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME};

use crate::util::error::{QuickResumeError, Result};

// CONTEXT_ALL for x64
const CONTEXT_ALL_X64: u32 = 0x0010_003F;
// WOW64_CONTEXT_ALL: i386 flag (0x10000) | CONTROL | INTEGER | SEGMENTS | FLOAT | DEBUG | EXTENDED
const WOW64_CONTEXT_ALL: u32 = 0x0001_003F;

/// Saved register state for a single thread.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ThreadSnapshot {
    pub tid: u32,
    /// Raw bytes of either CONTEXT (x64) or WOW64_CONTEXT (x86).
    /// Which one to use is determined by `SnapshotPayload::is_wow64`.
    pub context_bytes: Vec<u8>,
}

/// Capture the context of every thread belonging to `pid`.
///
/// Pass `is_wow64 = true` for 32-bit games to use Wow64GetThreadContext.
pub fn capture_thread_contexts(pid: u32, is_wow64: bool) -> Result<Vec<ThreadSnapshot>> {
    let tids = collect_tids(pid)?;
    let mut snapshots = Vec::with_capacity(tids.len());

    for tid in tids {
        match capture_one(tid, is_wow64) {
            Ok(snap) => snapshots.push(snap),
            Err(e) => {
                eprintln!("[threads] TID {}: capture failed — {}", tid, e);
            }
        }
    }

    Ok(snapshots)
}

fn capture_one(tid: u32, is_wow64: bool) -> Result<ThreadSnapshot> {
    let thread_handle =
        unsafe { OpenThread(THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME, false, tid) }
            .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let context_bytes = if is_wow64 {
        capture_wow64(tid, thread_handle)?
    } else {
        capture_x64(tid, thread_handle)?
    };

    unsafe { let _ = CloseHandle(thread_handle); }
    Ok(ThreadSnapshot { tid, context_bytes })
}

fn capture_x64(tid: u32, thread_handle: windows::Win32::Foundation::HANDLE) -> Result<Vec<u8>> {
    let mut ctx = AlignedX64Context::new();
    ctx.inner.ContextFlags =
        windows::Win32::System::Diagnostics::Debug::CONTEXT_FLAGS(CONTEXT_ALL_X64);

    unsafe { GetThreadContext(thread_handle, &mut ctx.inner) }
        .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &ctx.inner as *const CONTEXT as *const u8,
            mem::size_of::<CONTEXT>(),
        )
        .to_vec()
    };
    Ok(bytes)
}

fn capture_wow64(tid: u32, thread_handle: windows::Win32::Foundation::HANDLE) -> Result<Vec<u8>> {
    let mut ctx: WOW64_CONTEXT = unsafe { mem::zeroed() };
    ctx.ContextFlags = WOW64_CONTEXT_FLAGS(WOW64_CONTEXT_ALL);

    unsafe { Wow64GetThreadContext(thread_handle, &mut ctx) }
        .map_err(|e| QuickResumeError::ThreadContextFailed { tid, source: e })?;

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &ctx as *const WOW64_CONTEXT as *const u8,
            mem::size_of::<WOW64_CONTEXT>(),
        )
        .to_vec()
    };
    Ok(bytes)
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
