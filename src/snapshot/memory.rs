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

/// A snapshot of a single contiguous virtual memory region.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryRegion {
    /// Start address in the source process's virtual address space.
    pub base_address: u64,
    /// Size in bytes.
    pub size: usize,
    /// Win32 page-protection flags (e.g. PAGE_EXECUTE_READWRITE).
    pub protect: u32,
    /// Win32 region type (MEM_PRIVATE | MEM_MAPPED | MEM_IMAGE).
    pub region_type: u32,
    /// Raw bytes read from the process.
    pub data: Vec<u8>,
}

/// Regions that should never be snapshotted because they are reloaded from
/// disk by the OS loader (executable + read-only DLL sections).
/// Skipping them cuts snapshot size by 30–50 %.
fn should_skip(protect: u32, region_type: u32) -> bool {
    // Skip PAGE_NOACCESS — cannot be read
    if protect == PAGE_NOACCESS.0 {
        return true;
    }
    // Skip read-only image pages backed by EXE/DLL files on disk.
    // PAGE_READONLY and PAGE_EXECUTE_READ in MEM_IMAGE regions are
    // reconstructed by the loader when the process is re-launched.
    if region_type == MEM_IMAGE.0
        && (protect == PAGE_READONLY.0 || protect == PAGE_EXECUTE_READ.0)
    {
        return true;
    }
    false
}

/// Read all committed, readable, non-image-read-only regions from `handle`.
///
/// Returns each region as a [`MemoryRegion`] with its raw bytes.
pub fn dump_memory(handle: HANDLE) -> Result<Vec<MemoryRegion>> {
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

        let next_address = (mbi.BaseAddress as u64).saturating_add(mbi.RegionSize as u64);

        if mbi.State == MEM_COMMIT && mbi.Protect != PAGE_NOACCESS {
            let protect = mbi.Protect.0;
            let region_type = mbi.Type.0;

            if !should_skip(protect, region_type) {
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

                // ERROR_PARTIAL_COPY (0x8007012B): ReadProcessMemory read some
                // bytes but not all (guard-page boundary). Keep what we got.
                if bytes_read > 0 {
                    buf.truncate(bytes_read);
                    regions.push(MemoryRegion {
                        base_address: base,
                        size,
                        protect,
                        region_type,
                        data: buf,
                    });
                } else if let Err(e) = result {
                    eprintln!(
                        "[memory] ReadProcessMemory failed at 0x{:016X} ({}), skipping",
                        base, e
                    );
                }
            }
        }

        address = next_address;
        if address == 0 {
            break;
        }
    }

    Ok(regions)
}

pub fn print_stats(regions: &[MemoryRegion]) {
    let raw_bytes: u64 = regions.iter().map(|r| r.data.len() as u64).sum();
    println!(
        "[memory] Captured {} regions, {:.1} MB raw",
        regions.len(),
        raw_bytes as f64 / 1_048_576.0
    );
}
