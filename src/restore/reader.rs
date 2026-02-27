/// Read and decompress a `.qrs` snapshot file from disk.
use std::io::Read;
use std::path::Path;

use lz4_flex::decompress_size_prepended;

use crate::snapshot::writer::{SnapshotPayload, MAGIC, VERSION};
use crate::util::error::{QuickResumeError, Result};

/// Load and fully deserialize a snapshot from `path`.
pub fn read_snapshot(path: &Path) -> Result<SnapshotPayload> {
    let mut file = std::fs::File::open(path)?;

    // --- Header ---
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(QuickResumeError::InvalidSnapshotMagic);
    }

    let mut version_buf = [0u8; 4];
    file.read_exact(&mut version_buf)?;
    let version = u32::from_le_bytes(version_buf);
    if version != VERSION {
        return Err(QuickResumeError::UnsupportedVersion(version));
    }

    let mut ts_buf = [0u8; 8];
    file.read_exact(&mut ts_buf)?;
    let _timestamp_ms = u64::from_le_bytes(ts_buf);

    let mut size_buf = [0u8; 8];
    file.read_exact(&mut size_buf)?;
    let uncompressed_size = u64::from_le_bytes(size_buf) as usize;

    // --- Compressed payload ---
    let mut compressed = Vec::new();
    file.read_to_end(&mut compressed)?;

    let raw = decompress_size_prepended(&compressed)
        .map_err(|e| QuickResumeError::Other(format!("LZ4 decompress failed: {}", e)))?;

    if raw.len() != uncompressed_size {
        return Err(QuickResumeError::Other(format!(
            "Size mismatch: expected {} bytes, got {}",
            uncompressed_size,
            raw.len()
        )));
    }

    let payload: SnapshotPayload = bincode::deserialize(&raw)?;
    Ok(payload)
}

/// Print a human-readable header summary without deserializing the full payload.
pub fn peek_snapshot_header(path: &Path) -> Result<()> {
    let mut file = std::fs::File::open(path)?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(QuickResumeError::InvalidSnapshotMagic);
    }

    let mut version_buf = [0u8; 4];
    file.read_exact(&mut version_buf)?;
    let version = u32::from_le_bytes(version_buf);

    let mut ts_buf = [0u8; 8];
    file.read_exact(&mut ts_buf)?;
    let timestamp_ms = u64::from_le_bytes(ts_buf);

    let mut size_buf = [0u8; 8];
    file.read_exact(&mut size_buf)?;
    let uncompressed_size = u64::from_le_bytes(size_buf);

    let file_size = std::fs::metadata(path)?.len();
    let compressed_size = file_size - 4 - 4 - 8 - 8; // subtract header

    use std::time::{Duration, UNIX_EPOCH};
    let dt = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
    println!("=== Snapshot Header ===");
    println!("  Magic   : {}", std::str::from_utf8(&magic).unwrap_or("?"));
    println!("  Version : {}", version);
    println!("  Created : {:?}", dt);
    println!(
        "  Payload : {:.1} MB uncompressed, {:.1} MB on disk (ratio {:.1}x)",
        uncompressed_size as f64 / 1_048_576.0,
        compressed_size as f64 / 1_048_576.0,
        uncompressed_size as f64 / compressed_size.max(1) as f64
    );
    Ok(())
}
