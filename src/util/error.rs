use thiserror::Error;

#[derive(Debug, Error)]
pub enum QuickResumeError {
    #[error("Process not found: {0}")]
    ProcessNotFound(String),

    #[error("Failed to open process (pid={pid}): {source}")]
    OpenProcessFailed {
        pid: u32,
        #[source]
        source: windows::core::Error,
    },

    #[error("WinAPI error: {0}")]
    WinApi(#[from] windows::core::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialize(#[from] bincode::Error),

    #[error("Snapshot file is corrupt or invalid magic")]
    InvalidSnapshotMagic,

    #[error("Unsupported snapshot version: {0}")]
    UnsupportedVersion(u32),

    #[error("Memory allocation failed at 0x{address:016x}")]
    AllocationFailed { address: u64 },

    #[error("ReadProcessMemory failed at 0x{address:016x}: {source}")]
    ReadMemoryFailed {
        address: u64,
        #[source]
        source: windows::core::Error,
    },

    #[error("WriteProcessMemory failed at 0x{address:016x}: {source}")]
    WriteMemoryFailed {
        address: u64,
        #[source]
        source: windows::core::Error,
    },

    #[error("Thread context error (tid={tid}): {source}")]
    ThreadContextFailed {
        tid: u32,
        #[source]
        source: windows::core::Error,
    },

    #[error("PE parsing error: {0}")]
    PeParse(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, QuickResumeError>;
