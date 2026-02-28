/// Restore snapshotted memory regions into a freshly launched process.
///
/// Strategy for each saved MemoryRegion:
///   1. Try VirtualAllocEx at the exact base address (MEM_RESERVE | MEM_COMMIT).
///   2. If alloc fails (region already mapped by the loader/DLLs), try writing
///      directly: VirtualProtectEx to PAGE_EXECUTE_READWRITE → WriteProcessMemory
///      → VirtualProtectEx back to original protection.
///   3. If the direct write also fails (e.g., kernel-mapped guard pages), log and skip.
///
/// Skipped regions are usually read-only OS/DLL pages that are identical to what
/// the loader already placed there, so skipping them is safe.
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, MEM_COMMIT, MEM_RELEASE,
    MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};

use crate::snapshot::memory::MemoryRegion;
use crate::util::error::{QuickResumeError, Result};

/// Write all regions from the snapshot into `target_handle`.
///
/// Failures on individual regions are logged and counted. Skipped regions
/// (where neither alloc nor direct-write succeeded) are expected for OS-managed
/// pages and are not treated as fatal unless they exceed the threshold.
pub fn restore_memory(target_handle: HANDLE, regions: &[MemoryRegion]) -> Result<()> {
    let mut alloc_ok = 0usize;
    let mut fallback_ok = 0usize;
    let mut skipped = 0usize;

    for region in regions {
        match restore_region(target_handle, region) {
            Ok(RestoreOutcome::AllocatedAndWritten) => alloc_ok += 1,
            Ok(RestoreOutcome::DirectWritten) => fallback_ok += 1,
            Ok(RestoreOutcome::Skipped) => {
                skipped += 1;
            }
            Err(e) => {
                // WriteProcessMemory error after a successful alloc — unexpected.
                eprintln!(
                    "[restore::memory] 0x{:016X} ({} bytes): {}",
                    region.base_address, region.size, e
                );
                skipped += 1;
            }
        }
    }

    let total = regions.len();
    println!(
        "[restore::memory] {} allocated+written, {} fallback-written, {} skipped / {} total",
        alloc_ok, fallback_ok, skipped, total
    );

    // Fatal only if we couldn't write the majority of regions.
    // A high skip count is expected for WOW64 processes (OS DLLs, guard pages).
    if skipped * 2 > total {
        return Err(QuickResumeError::Other(format!(
            "Too many regions could not be restored ({}/{} skipped)",
            skipped,
            total
        )));
    }

    Ok(())
}

enum RestoreOutcome {
    AllocatedAndWritten,
    DirectWritten,
    Skipped,
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
            // Allocation landed at a wrong address — free it and fall through to path B.
            unsafe { let _ = VirtualFreeEx(handle, alloc, 0, MEM_RELEASE); }
        } else {
            // Write the saved bytes.
            let write_ok = unsafe {
                WriteProcessMemory(
                    handle,
                    base_ptr,
                    region.data.as_ptr() as *const _,
                    write_len,
                    None,
                )
            };

            // Restore original page protection (best-effort).
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

    // ── Path B: region already mapped — try to write in-place ──────────────
    //
    // This covers DLL pages, OS-loader regions, and pages that were already
    // committed in the new process at the right address.
    let mut old_protect = PAGE_PROTECTION_FLAGS(0);

    // Make the existing mapping writable (ignore failure — it might already be writable).
    let _ = unsafe {
        VirtualProtectEx(
            handle,
            base_ptr,
            write_len.max(1),
            PAGE_EXECUTE_READWRITE,
            &mut old_protect,
        )
    };

    // Attempt the write.
    let write_ok = unsafe {
        WriteProcessMemory(
            handle,
            base_ptr,
            region.data.as_ptr() as *const _,
            write_len,
            None,
        )
    };

    // Restore protection whether or not the write succeeded.
    let _ = unsafe {
        VirtualProtectEx(
            handle,
            base_ptr,
            write_len.max(1),
            PAGE_PROTECTION_FLAGS(region.protect),
            &mut old_protect,
        )
    };

    match write_ok {
        Ok(()) => Ok(RestoreOutcome::DirectWritten),
        Err(_) => Ok(RestoreOutcome::Skipped), // kernel pages, guard pages — safe to skip
    }
}
