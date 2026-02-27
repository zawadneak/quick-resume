/// Serialize and compress a full process snapshot to disk.
///
/// File format:
/// ```text
/// [4 bytes]  Magic: b"QRSV"
/// [4 bytes]  Version: u32 (little-endian)
/// [8 bytes]  Timestamp: u64 unix milliseconds (little-endian)
/// [8 bytes]  Uncompressed payload size: u64 (little-endian)
/// [N bytes]  LZ4-compressed bincode payload
/// ```
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use lz4_flex::compress_prepend_size;
use serde::{Deserialize, Serialize};

use crate::snapshot::memory::MemoryRegion;
use crate::snapshot::threads::ThreadSnapshot;
use crate::util::error::Result;

pub const MAGIC: &[u8; 4] = b"QRSV";
pub const VERSION: u32 = 1;

/// The full snapshot payload that gets serialized + compressed.
#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub process_name: String,
    pub pid: u32,
    pub timestamp_ms: u64,
    pub memory_regions: Vec<MemoryRegion>,
    pub thread_snapshots: Vec<ThreadSnapshot>,
}

/// Write the snapshot to `path` (e.g. `snapshot/mini_metro.qrs`).
///
/// Returns (raw_bytes, compressed_bytes).
pub fn write_snapshot(payload: &SnapshotPayload, path: &Path) -> Result<(u64, u64)> {
    // Create parent directory if needed.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Serialize payload with bincode.
    let raw_bytes = bincode::serialize(payload)?;
    let raw_size = raw_bytes.len() as u64;

    // Compress with LZ4.
    let compressed = compress_prepend_size(&raw_bytes);
    let compressed_size = compressed.len() as u64;

    // Write file.
    let mut file = std::fs::File::create(path)?;
    file.write_all(MAGIC)?;
    file.write_all(&VERSION.to_le_bytes())?;
    file.write_all(&payload.timestamp_ms.to_le_bytes())?;
    file.write_all(&raw_size.to_le_bytes())?;
    file.write_all(&compressed)?;
    file.flush()?;

    Ok((raw_size, compressed_size))
}

/// Convenience: build a [`SnapshotPayload`] from captured data.
pub fn build_payload(
    process_name: &str,
    pid: u32,
    memory_regions: Vec<MemoryRegion>,
    thread_snapshots: Vec<ThreadSnapshot>,
) -> SnapshotPayload {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    SnapshotPayload {
        process_name: process_name.to_string(),
        pid,
        timestamp_ms,
        memory_regions,
        thread_snapshots,
    }
}
