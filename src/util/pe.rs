/// PE header utilities for ASLR manipulation.
///
/// IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE = 0x0040
/// Clearing this flag disables ASLR so the executable loads at its preferred
/// base address on every launch — required for deterministic memory restore.
use std::fs;
use std::path::Path;

use crate::util::error::{QuickResumeError, Result};

const IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE: u16 = 0x0040;
const MZ_MAGIC: u16 = 0x5A4D; // "MZ"
const PE_MAGIC: u32 = 0x0000_4550; // "PE\0\0"

/// Returns the current DllCharacteristics field value for the given PE file.
pub fn read_dll_characteristics(path: &Path) -> Result<u16> {
    let data = fs::read(path)?;
    let chars_offset = find_dll_characteristics_offset(&data)?;
    let value = u16::from_le_bytes([data[chars_offset], data[chars_offset + 1]]);
    Ok(value)
}

/// Clears IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE in the PE header on disk.
/// Creates a `.bak` backup next to the original before patching.
///
/// # Safety
/// This modifies the file on disk. Keep a backup (`Mini Metro.exe.bak`).
pub fn disable_aslr(path: &Path) -> Result<()> {
    // Backup original
    let backup_path = path.with_extension("exe.bak");
    if !backup_path.exists() {
        fs::copy(path, &backup_path)?;
        println!(
            "[pe] Backup written to {}",
            backup_path.display()
        );
    } else {
        println!("[pe] Backup already exists, skipping copy.");
    }

    let mut data = fs::read(path)?;
    let chars_offset = find_dll_characteristics_offset(&data)?;

    let before = u16::from_le_bytes([data[chars_offset], data[chars_offset + 1]]);
    let after = before & !IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE;

    if before == after {
        println!("[pe] ASLR already disabled (DllCharacteristics=0x{:04X})", before);
        return Ok(());
    }

    data[chars_offset] = (after & 0xFF) as u8;
    data[chars_offset + 1] = ((after >> 8) & 0xFF) as u8;

    fs::write(path, &data)?;
    println!(
        "[pe] DllCharacteristics patched: 0x{:04X} → 0x{:04X} (ASLR disabled)",
        before, after
    );
    Ok(())
}

/// Restores ASLR by copying the `.bak` file back over the patched executable.
pub fn restore_aslr(path: &Path) -> Result<()> {
    let backup_path = path.with_extension("exe.bak");
    if !backup_path.exists() {
        return Err(QuickResumeError::PeParse(format!(
            "No backup found at {}",
            backup_path.display()
        )));
    }
    fs::copy(&backup_path, path)?;
    println!("[pe] Restored original executable from backup.");
    Ok(())
}

/// Returns the byte offset of the DllCharacteristics field within `data`.
fn find_dll_characteristics_offset(data: &[u8]) -> Result<usize> {
    if data.len() < 64 {
        return Err(QuickResumeError::PeParse("File too small to be a PE".into()));
    }

    let mz = u16::from_le_bytes([data[0], data[1]]);
    if mz != MZ_MAGIC {
        return Err(QuickResumeError::PeParse("Not a valid PE file (missing MZ)".into()));
    }

    // e_lfanew is at offset 0x3C
    let e_lfanew = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;

    if e_lfanew + 4 > data.len() {
        return Err(QuickResumeError::PeParse("e_lfanew out of range".into()));
    }

    let pe_sig = u32::from_le_bytes([
        data[e_lfanew],
        data[e_lfanew + 1],
        data[e_lfanew + 2],
        data[e_lfanew + 3],
    ]);
    if pe_sig != PE_MAGIC {
        return Err(QuickResumeError::PeParse("Missing PE signature".into()));
    }

    // IMAGE_FILE_HEADER is 20 bytes after PE signature
    // IMAGE_OPTIONAL_HEADER starts at e_lfanew + 4 + 20 = e_lfanew + 24
    // DllCharacteristics is at offset 70 within IMAGE_OPTIONAL_HEADER (for PE32+/x64)
    // PE32+: Magic=0x020B, DllCharacteristics at optional_header_base + 70
    let optional_header_base = e_lfanew + 4 + 20;
    if optional_header_base + 2 > data.len() {
        return Err(QuickResumeError::PeParse("Optional header out of range".into()));
    }

    let opt_magic = u16::from_le_bytes([data[optional_header_base], data[optional_header_base + 1]]);
    let dll_chars_offset = match opt_magic {
        0x010B => optional_header_base + 70,  // PE32
        0x020B => optional_header_base + 70,  // PE32+ (x64) — same offset
        _ => {
            return Err(QuickResumeError::PeParse(format!(
                "Unknown optional header magic: 0x{:04X}",
                opt_magic
            )))
        }
    };

    if dll_chars_offset + 2 > data.len() {
        return Err(QuickResumeError::PeParse(
            "DllCharacteristics field out of range".into(),
        ));
    }

    Ok(dll_chars_offset)
}
