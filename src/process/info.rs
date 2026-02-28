/// Enumerate memory regions, threads, and loaded modules of a process.
use std::mem;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Thread32First, Thread32Next,
    MODULEENTRY32W, THREADENTRY32, TH32CS_SNAPMODULE, TH32CS_SNAPTHREAD,
};
use windows::Win32::System::Memory::{
    VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_FREE, MEM_MAPPED,
};

use crate::util::error::Result;

// ── Memory regions ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct RegionInfo {
    pub base_address: u64,
    pub size: usize,
    pub state: u32,
    pub protect: u32,
    pub region_type: u32, // MEM_PRIVATE | MEM_MAPPED | MEM_IMAGE
}

/// Walk all virtual memory regions of the process and return their descriptors.
pub fn enumerate_memory_regions(handle: HANDLE) -> Result<Vec<RegionInfo>> {
    let mut regions = Vec::new();
    let mut address: u64 = 0;

    loop {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let ret = unsafe {
            VirtualQueryEx(
                handle,
                Some(address as *const _),
                &mut mbi,
                mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if ret == 0 {
            break;
        }

        regions.push(RegionInfo {
            base_address: mbi.BaseAddress as u64,
            size: mbi.RegionSize,
            state: mbi.State.0,
            protect: mbi.Protect.0,
            region_type: mbi.Type.0,
        });

        address = (mbi.BaseAddress as u64).saturating_add(mbi.RegionSize as u64);
        if address == 0 {
            break; // wrapped around
        }
    }

    Ok(regions)
}

// ── Threads ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ThreadInfo {
    pub tid: u32,
    pub base_priority: i32,
}

/// Enumerate all threads belonging to `pid`.
pub fn enumerate_threads(pid: u32) -> Result<Vec<ThreadInfo>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?;

    let mut threads = Vec::new();
    let mut entry = THREADENTRY32 {
        dwSize: mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };

    unsafe {
        if Thread32First(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32OwnerProcessID == pid {
                    threads.push(ThreadInfo {
                        tid: entry.th32ThreadID,
                        base_priority: entry.tpBasePri,
                    });
                }
                if Thread32Next(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }

    Ok(threads)
}

// ── Modules ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ModuleInfo {
    pub name: String,
    pub base_address: u64,
    pub size: u32,
    pub path: String,
}

/// Enumerate all loaded modules (DLLs) in the process.
pub fn enumerate_modules(pid: u32) -> Result<Vec<ModuleInfo>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid) }?;

    let mut modules = Vec::new();
    let mut entry = MODULEENTRY32W {
        dwSize: mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };

    unsafe {
        if Module32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = wide_to_string(&entry.szModule);
                let path = wide_to_string(&entry.szExePath);
                modules.push(ModuleInfo {
                    name,
                    base_address: entry.modBaseAddr as u64,
                    size: entry.modBaseSize,
                    path,
                });
                if Module32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }

    Ok(modules)
}

// ── Summary printer ───────────────────────────────────────────────────────────

pub struct ProcessSummary {
    pub pid: u32,
    pub name: String,
    pub total_committed_bytes: u64,
    pub committed_region_count: usize,
    pub free_region_count: usize,
    pub mapped_region_count: usize,
    pub thread_count: usize,
    pub module_count: usize,
    pub modules: Vec<ModuleInfo>,
    pub threads: Vec<ThreadInfo>,
}

pub fn build_summary(
    handle: HANDLE,
    pid: u32,
    name: &str,
) -> Result<ProcessSummary> {
    let regions = enumerate_memory_regions(handle)?;
    let threads = enumerate_threads(pid)?;
    let modules = enumerate_modules(pid)?;

    let committed: Vec<_> = regions
        .iter()
        .filter(|r| r.state == MEM_COMMIT.0)
        .collect();
    let free_count = regions.iter().filter(|r| r.state == MEM_FREE.0).count();
    let mapped_count = regions
        .iter()
        .filter(|r| r.state == MEM_COMMIT.0 && r.region_type == MEM_MAPPED.0)
        .count();
    let total_committed: u64 = committed.iter().map(|r| r.size as u64).sum();

    Ok(ProcessSummary {
        pid,
        name: name.to_string(),
        total_committed_bytes: total_committed,
        committed_region_count: committed.len(),
        free_region_count: free_count,
        mapped_region_count: mapped_count,
        thread_count: threads.len(),
        module_count: modules.len(),
        modules,
        threads,
    })
}

pub fn print_summary(s: &ProcessSummary) {
    println!("=== Process Report: {} (PID {}) ===", s.name, s.pid);
    println!(
        "  Committed regions : {}  ({:.1} MB)",
        s.committed_region_count,
        s.total_committed_bytes as f64 / 1_048_576.0
    );
    println!("  Free regions      : {}", s.free_region_count);
    println!("  Memory-mapped     : {}", s.mapped_region_count);
    println!("  Threads           : {}", s.thread_count);
    println!("  Loaded modules    : {}", s.module_count);
    println!();
    println!("--- Modules ---");
    for m in &s.modules {
        println!(
            "  {:<40} base=0x{:016X}  size={:.1} KB",
            m.name,
            m.base_address,
            m.size as f64 / 1024.0
        );
    }
    println!();
    println!("--- Threads ---");
    for t in &s.threads {
        println!("  TID {:>6}  base_priority={}", t.tid, t.base_priority);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
