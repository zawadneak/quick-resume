/// Restore snapshotted memory regions into a freshly launched process.
///
/// Strategy for each saved MemoryRegion:
///   1. Try VirtualAllocEx at the exact base address (MEM_RESERVE | MEM_COMMIT).
///   2. If alloc fails (region already mapped by the loader/DLLs), try writing
///      directly: VirtualProtectEx to PAGE_EXECUTE_READWRITE → WriteProcessMemory
///      → VirtualProtectEx back to original protection.
///   3. If the direct write also fails, use VirtualQueryEx to diagnose WHY and
///      categorize the skip.
///
/// A categorized skip summary is printed at the end.
use std::mem;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_IMAGE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    PAGE_PROTECTION_FLAGS, VIRTUAL_FREE_TYPE, VirtualAllocEx, VirtualFreeEx,
    VirtualProtectEx, VirtualQueryEx, MEMORY_BASIC_INFORMATION,
};

use crate::snapshot::memory::MemoryRegion;
use crate::util::error::{QuickResumeError, Result};

const PAGE_GUARD_FLAG: u32 = 0x100;

/// Write all regions from the snapshot into `target_handle`.
pub fn restore_memory(target_handle: HANDLE, regions: &[MemoryRegion]) -> Result<()> {
    let mut alloc_ok = 0usize;
    let mut fallback_ok = 0usize;

    // Skip categories
    let mut skip_guard = 0usize;      // target page is a guard page
    let mut skip_image = 0usize;      // target is a MEM_IMAGE (DLL) page — OS loads it from disk
    let mut skip_not_mapped = 0usize; // address not committed in target at all
    let mut skip_write_err = 0usize;  // WriteProcessMemory failed (kernel range, etc.)

    for region in regions {
        match restore_region(target_handle, region) {
            Ok(RestoreOutcome::AllocatedAndWritten) => alloc_ok += 1,
            Ok(RestoreOutcome::DirectWritten) => fallback_ok += 1,
            Ok(RestoreOutcome::SkipGuard) => skip_guard += 1,
            Ok(RestoreOutcome::SkipImage) => skip_image += 1,
            Ok(RestoreOutcome::SkipNotMapped) => skip_not_mapped += 1,
            Ok(RestoreOutcome::SkipWriteError(code)) => {
                skip_write_err += 1;
                // Only print the first ~10 unexpected write errors to avoid log spam.
                if skip_write_err <= 10 {
                    eprintln!(
                        "[restore::memory] Write error at 0x{:016X} ({} bytes) err=0x{:08X}",
                        region.base_address, region.data.len(), code
                    );
                }
            }
            Err(e) => {
                // WriteProcessMemory failed after a successful fresh alloc — unexpected.
                eprintln!(
                    "[restore::memory] Unexpected error at 0x{:016X}: {}",
                    region.base_address, e
                );
                skip_write_err += 1;
            }
        }
    }

    let total = regions.len();
    let total_skip = skip_guard + skip_image + skip_not_mapped + skip_write_err;

    println!(
        "[restore::memory] Results ({} regions):",
        total
    );
    println!(
        "  Restored: {} fresh-alloc + {} fallback-overwrite = {} total",
        alloc_ok, fallback_ok, alloc_ok + fallback_ok
    );
    println!(
        "  Skipped:  {} guard-pages, {} image/DLL, {} not-mapped, {} write-errors = {} total",
        skip_guard, skip_image, skip_not_mapped, skip_write_err, total_skip
    );

    // not-mapped: snapshot has regions at DLL addresses that ASLR moved in the
    // new process — those addresses are simply MEM_FREE there.
    // write-errors: kernel/OS-managed low-range pages (PEB, TEB, loader) that
    // can't be written from userspace.
    //
    // Neither is fatal: Unity game state lives in MEM_PRIVATE (managed heap +
    // JIT code), which is restored via fresh-alloc. DLL data sections reload
    // from disk and Unity re-initializes them from the managed heap.
    if skip_not_mapped + skip_write_err > 0 {
        eprintln!(
            "[restore::memory] Warning: {} DLL-ASLR gaps + {} kernel-page errors — \
             game state in managed heap should still be intact.",
            skip_not_mapped, skip_write_err
        );
    }

    Ok(())
}

enum RestoreOutcome {
    AllocatedAndWritten,
    DirectWritten,
    SkipGuard,
    SkipImage,
    SkipNotMapped,
    SkipWriteError(u32),
}

fn restore_region(handle: HANDLE, region: &MemoryRegion) -> Result<RestoreOutcome> {
    let base_ptr = region.base_address as *const std::ffi::c_void;
    let write_len = region.data.len();

    // ── Path A: allocate fresh pages at the exact address ──────────────────
    let alloc = unsafe {
        VirtualAllocEx(
            handle,
            Some(base_ptr),
            region.size,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_EXECUTE_READWRITE,
        )
    };

    if !alloc.is_null() {
        if alloc as usize != region.base_address as usize {
            // Allocation landed at a wrong address — free it and fall through.
            unsafe { let _ = VirtualFreeEx(handle, alloc, 0, VIRTUAL_FREE_TYPE(0x8000)); } // MEM_RELEASE = 0x8000
        } else {
            let write_ok = unsafe {
                WriteProcessMemory(
                    handle,
                    base_ptr,
                    region.data.as_ptr() as *const _,
                    write_len,
                    None,
                )
            };

            let mut old_protect = PAGE_PROTECTION_FLAGS(0);
            let _ = unsafe {
                VirtualProtectEx(
                    handle,
                    base_ptr,
                    write_len.max(1),
                    PAGE_PROTECTION_FLAGS(region.protect),
                    &mut old_protect,
                )
            };

            return match write_ok {
                Ok(()) => Ok(RestoreOutcome::AllocatedAndWritten),
                Err(e) => Err(QuickResumeError::WriteMemoryFailed {
                    address: region.base_address,
                    source: e,
                }),
            };
        }
    }

    // ── Path B: region already mapped — query target to understand it ───────
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let queried = unsafe {
        VirtualQueryEx(
            handle,
            Some(base_ptr),
            &mut mbi,
            mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };

    if queried == 0 || mbi.State == MEM_FREE {
        // Address is not mapped at all in the target process.
        return Ok(RestoreOutcome::SkipNotMapped);
    }

    // Guard page in the target? These regenerate naturally.
    if mbi.Protect.0 & PAGE_GUARD_FLAG != 0 {
        return Ok(RestoreOutcome::SkipGuard);
    }

    // MEM_IMAGE page: DLL/EXE code/data loaded from disk by the OS.
    // We can try to write but often can't change protection.
    // If the snapshot region is also MEM_IMAGE, the OS will have loaded the
    // same bytes from disk — safe to skip.
    if mbi.Type == MEM_IMAGE && region.region_type == MEM_IMAGE.0 {
        return Ok(RestoreOutcome::SkipImage);
    }

    // ── Path B continued: attempt in-place overwrite ────────────────────────
    let mut old_protect = PAGE_PROTECTION_FLAGS(0);

    // Make writable (best-effort — fails for some kernel/image pages).
    let _ = unsafe {
        VirtualProtectEx(
            handle,
            base_ptr,
            write_len.max(1),
            PAGE_EXECUTE_READWRITE,
            &mut old_protect,
        )
    };

    let write_result = unsafe {
        WriteProcessMemory(
            handle,
            base_ptr,
            region.data.as_ptr() as *const _,
            write_len,
            None,
        )
    };

    // Restore protection (best-effort).
    let _ = unsafe {
        VirtualProtectEx(
            handle,
            base_ptr,
            write_len.max(1),
            PAGE_PROTECTION_FLAGS(region.protect),
            &mut old_protect,
        )
    };

    match write_result {
        Ok(()) => Ok(RestoreOutcome::DirectWritten),
        Err(e) => {
            let code = e.code().0 as u32;
            // Distinguish image pages that we couldn't overwrite (expected)
            // from truly unexpected failures.
            if mbi.Type == MEM_IMAGE {
                Ok(RestoreOutcome::SkipImage)
            } else {
                Ok(RestoreOutcome::SkipWriteError(code))
            }
        }
    }
}
