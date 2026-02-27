/// Restore snapshotted memory regions into a freshly launched process.
///
/// Phase 3d of the restore plan:
///   For each saved MemoryRegion:
///     1. VirtualAllocEx at the exact base address (MEM_RESERVE | MEM_COMMIT, MEM_FIXED).
///     2. WriteProcessMemory to copy the saved bytes.
///     3. VirtualProtectEx to restore the original protection flags.
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, MEM_COMMIT, MEM_FREE, MEM_RELEASE,
    MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};

use crate::snapshot::memory::MemoryRegion;
use crate::util::error::{QuickResumeError, Result};

/// Write all regions from the snapshot into `target_handle`.
///
/// Failures on individual regions are logged and counted rather than
/// immediately aborting, so we restore as much state as possible before
/// deciding whether to give up.
pub fn restore_memory(target_handle: HANDLE, regions: &[MemoryRegion]) -> Result<()> {
    let mut failed = 0usize;

    for region in regions {
        if let Err(e) = restore_region(target_handle, region) {
            eprintln!(
                "[restore::memory] 0x{:016X} ({} bytes): {}",
                region.base_address, region.size, e
            );
            failed += 1;
        }
    }

    if failed > 0 {
        eprintln!(
            "[restore::memory] {} / {} regions failed.",
            failed,
            regions.len()
        );
    } else {
        println!(
            "[restore::memory] All {} regions restored successfully.",
            regions.len()
        );
    }

    // Treat failures on more than 5 % of regions as fatal.
    if failed * 20 > regions.len() {
        return Err(QuickResumeError::Other(format!(
            "Too many region restore failures ({}/{})",
            failed,
            regions.len()
        )));
    }

    Ok(())
}

fn restore_region(handle: HANDLE, region: &MemoryRegion) -> Result<()> {
    let base = region.base_address as usize;
    let size = region.size;

    // Allocate at the exact virtual address.
    // PAGE_EXECUTE_READWRITE gives us write access initially; we restore the
    // correct protection after writing.
    let alloc = unsafe {
        VirtualAllocEx(
            handle,
            Some(base as *const _),
            size,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_EXECUTE_READWRITE,
        )
    };

    if alloc.is_null() {
        return Err(QuickResumeError::AllocationFailed {
            address: region.base_address,
        });
    }

    // Write the saved bytes.
    let mut bytes_written = 0usize;
    unsafe {
        WriteProcessMemory(
            handle,
            base as *const _,
            region.data.as_ptr() as *const _,
            region.data.len(),
            Some(&mut bytes_written),
        )
    }
    .map_err(|e| QuickResumeError::WriteMemoryFailed {
        address: region.base_address,
        source: e,
    })?;

    // Restore original page protection.
    let mut old_protect = PAGE_PROTECTION_FLAGS(0);
    unsafe {
        VirtualProtectEx(
            handle,
            base as *const _,
            size,
            PAGE_PROTECTION_FLAGS(region.protect),
            &mut old_protect,
        )
    }
    .map_err(|e| QuickResumeError::WriteMemoryFailed {
        address: region.base_address,
        source: e,
    })?;

    Ok(())
}

/// Free a virtual region that was previously allocated in `handle`.
/// Used during cleanup if restore fails midway.
pub fn free_region(handle: HANDLE, base_address: u64) -> Result<()> {
    unsafe {
        VirtualFreeEx(handle, base_address as *mut _, 0, MEM_RELEASE)?;
    }
    Ok(())
}
