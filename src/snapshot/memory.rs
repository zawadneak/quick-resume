/// Dump all writable committed memory regions from a running (suspended) process.
use std::mem;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Memory::{
    VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE,
    PAGE_EXECUTE_READ, PAGE_NOACCESS, PAGE_READONLY,
};

use crate::util::error::Result;

// PAGE_GUARD is a protection modifier (0x100) — present on stack-growth guard
// pages and Unity heap segment guards. They trigger a one-shot exception on
// first access and are re-created by the OS for each new process.
const PAGE_GUARD_FLAG: u32 = 0x100;

/// A snapshot of a single contiguous virtual memory region.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryRegion {
    /// Start address in the source process's virtual address space.
    pub base_address: u64,
    /// Full size (in bytes) of the VirtualQueryEx region.
    pub size: usize,
    /// Win32 page-protection flags (e.g. PAGE_EXECUTE_READWRITE).
    pub protect: u32,
    /// Win32 region type (MEM_PRIVATE | MEM_MAPPED | MEM_IMAGE).
    pub region_type: u32,
    /// Raw bytes read from the process (may be shorter than `size` on partial reads).
    pub data: Vec<u8>,
}


fn type_str(region_type: u32) -> &'static str {
    match region_type {
        t if t == MEM_IMAGE.0 => "image",
        0x0002_0000 => "private",   // MEM_PRIVATE
        0x0004_0000 => "mapped",    // MEM_MAPPED
        _ => "?",
    }
}

fn prot_str(protect: u32) -> String {
    let base = protect & 0xFF;
    let base_s = match base {
        0x01 => "NOACCESS",
        0x02 => "READONLY",
        0x04 => "READWRITE",
        0x08 => "WRITECOPY",
        0x10 => "EXECUTE",
        0x20 => "EXECUTE_READ",
        0x40 => "EXECUTE_READWRITE",
        0x80 => "EXECUTE_WRITECOPY",
        _ => "?",
    };
    let mut mods = String::new();
    if protect & 0x100 != 0 { mods.push_str("+GUARD"); }
    if protect & 0x200 != 0 { mods.push_str("+NOCACHE"); }
    if protect & 0x400 != 0 { mods.push_str("+WRITECOMBINE"); }
    format!("{}{}", base_s, mods)
}

/// Read all committed, readable, non-image-read-only regions from `handle`.
pub fn dump_memory(handle: HANDLE) -> Result<Vec<MemoryRegion>> {
    let mut regions = Vec::new();
    let mut address: u64 = 0;

    let mut skipped_guard = 0u32;
    let mut skipped_noaccess = 0u32;
    let mut skipped_image_ro = 0u32;
    let mut read_partial = 0u32;   // bytes_read > 0 but < size
    let mut read_failed = 0u32;    // bytes_read == 0

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

        let next_address = (mbi.BaseAddress as u64).saturating_add(mbi.RegionSize as u64);

        if mbi.State == MEM_COMMIT {
            let protect = mbi.Protect.0;
            let region_type = mbi.Type.0;

            if protect == PAGE_NOACCESS.0 {
                skipped_noaccess += 1;
            } else if protect & PAGE_GUARD_FLAG != 0 {
                skipped_guard += 1;
            } else if region_type == MEM_IMAGE.0
                && (protect == PAGE_READONLY.0 || protect == PAGE_EXECUTE_READ.0)
            {
                skipped_image_ro += 1;
            } else {
                let base = mbi.BaseAddress as u64;
                let size = mbi.RegionSize;
                let mut buf = vec![0u8; size];
                let mut bytes_read = 0usize;

                let result = unsafe {
                    ReadProcessMemory(
                        handle,
                        base as *const _,
                        buf.as_mut_ptr() as *mut _,
                        size,
                        Some(&mut bytes_read),
                    )
                };

                if bytes_read == size {
                    // Perfect read.
                    regions.push(MemoryRegion {
                        base_address: base,
                        size,
                        protect,
                        region_type,
                        data: buf,
                    });
                } else if bytes_read > 0 {
                    // Partial read — guard page boundary mid-region.
                    read_partial += 1;
                    eprintln!(
                        "[memory] Partial read at 0x{:016X}: {} / {} bytes  type={} prot={}",
                        base, bytes_read, size, type_str(region_type), prot_str(protect)
                    );
                    buf.truncate(bytes_read);
                    regions.push(MemoryRegion {
                        base_address: base,
                        size,
                        protect,
                        region_type,
                        data: buf,
                    });
                } else {
                    // Complete failure — log with full region metadata.
                    read_failed += 1;
                    let err_code = result.err().map(|e| e.code().0 as u32).unwrap_or(0);
                    eprintln!(
                        "[memory] Read FAILED at 0x{:016X} size={} type={} prot={} err=0x{:08X}",
                        base, size, type_str(region_type), prot_str(protect), err_code
                    );
                }
            }
        }

        address = next_address;
        if address == 0 {
            break;
        }
    }

    println!(
        "[memory] Skipped: {} noaccess, {} guard, {} image-ro  |  Failed reads: {} partial (kept), {} total-fail",
        skipped_noaccess, skipped_guard, skipped_image_ro, read_partial, read_failed
    );

    Ok(regions)
}

pub fn print_stats(regions: &[MemoryRegion]) {
    let raw_bytes: u64 = regions.iter().map(|r| r.data.len() as u64).sum();
    let image_count = regions.iter().filter(|r| r.region_type == MEM_IMAGE.0).count();
    let private_count = regions.iter().filter(|r| r.region_type == 0x0002_0000).count();
    let mapped_count = regions.iter().filter(|r| r.region_type == 0x0004_0000).count();
    println!(
        "[memory] Captured {} regions ({} image-writable, {} private, {} mapped), {:.1} MB raw",
        regions.len(),
        image_count,
        private_count,
        mapped_count,
        raw_bytes as f64 / 1_048_576.0
    );
}
