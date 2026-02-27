# Quick Resume

Xbox-style Quick Resume for Windows — snapshot a running process's full state (memory + threads) to disk and restore it later from a cold launch.

## Project Overview

A Rust CLI tool that can freeze a running Windows process, dump its entire virtual memory and thread register contexts to a compressed file, and later restore that exact state into a freshly spawned process. The primary use case is games (tested with Mini Metro / Unity titles), enabling instant resume without the game's normal startup sequence.

## Architecture

```
src/
├── main.rs              # CLI entry point, orchestrates all commands
├── process/             # Attach to and inspect live processes
│   ├── attach.rs        # Find process by name via Toolhelp32, open with PROCESS_ALL_ACCESS
│   ├── info.rs          # Enumerate memory regions, threads, and loaded modules
│   └── suspend.rs       # RAII SuspendGuard using NtSuspendProcess/NtResumeProcess
├── snapshot/            # Capture process state
│   ├── memory.rs        # Walk VirtualQueryEx, ReadProcessMemory for all committed regions
│   ├── threads.rs       # GetThreadContext (CONTEXT_ALL) for every thread
│   └── writer.rs        # Serialize with bincode, compress with LZ4, write .qrs file
├── restore/             # Rebuild process from snapshot
│   ├── launch.rs        # CreateProcessW(CREATE_SUSPENDED), wait for loader (ntdll+kernel32)
│   ├── memory.rs        # VirtualAllocEx at exact addresses, WriteProcessMemory, VirtualProtectEx
│   ├── reader.rs        # Read and decompress .qrs file, peek header
│   └── threads.rs       # SetThreadContext on new process threads (matched by index)
└── util/
    ├── error.rs         # QuickResumeError enum (thiserror), covers all failure modes
    └── pe.rs            # Disable/restore ASLR by patching IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE
```

## CLI Commands

| Command | Description |
|---|---|
| `--profile [--target <exe>]` | Enumerate and print memory regions, threads, and modules of a running process |
| `--snapshot [--target <exe>] [--out <path>]` | Suspend process, dump memory + threads, compress, write `.qrs` file |
| `--restore [--exe <path>] [--out <path>]` | Read snapshot, launch exe suspended, inject memory + thread contexts, resume |
| `--disable-aslr <path>` | Patch PE header to clear DYNAMIC_BASE flag (creates `.bak` backup) |
| `--restore-aslr <path>` | Restore original PE from `.bak` backup |
| `--peek` | Print snapshot file header info without full deserialization |

Default target: `Mini Metro.exe`. Default snapshot path: `snapshot/mini_metro.qrs`.

## Snapshot File Format (`.qrs`)

```
[4 bytes]  Magic: b"QRSV"
[4 bytes]  Version: u32 LE (currently 1)
[8 bytes]  Timestamp: u64 unix ms LE
[8 bytes]  Uncompressed size: u64 LE
[N bytes]  LZ4-compressed bincode payload (SnapshotPayload)
```

`SnapshotPayload` contains: process name, PID, timestamp, `Vec<MemoryRegion>` (base address, size, protect flags, type, raw bytes), `Vec<ThreadSnapshot>` (TID, raw CONTEXT bytes).

## Key Design Decisions

- **Memory filtering**: Skips `PAGE_NOACCESS` regions and read-only `MEM_IMAGE` pages (PE sections reloaded by the OS loader), reducing snapshot size ~30-50%.
- **ASLR patching**: The executable must load at its preferred base address for memory addresses to match. The `--disable-aslr` command patches the PE on disk before snapshotting. A `.bak` backup is always created.
- **Thread context matching**: Threads in the new process are matched to saved contexts by creation order (index), not by TID. This works for deterministic process startup (e.g., Unity games).
- **Loader wait**: After `CREATE_SUSPENDED`, the main thread is briefly resumed in a loop (up to 1s) until `ntdll.dll` and `kernel32.dll` appear in the module list, ensuring the OS loader has initialized the heap and system DLLs.
- **Graceful degradation**: Memory region restore failures are logged and counted; only >5% failure rate is treated as fatal. Thread capture errors are also logged and skipped individually.
- **SuspendGuard**: RAII pattern ensures `NtResumeProcess` is always called, even on panic/early return, so the target process is never left permanently frozen.
- **Compression**: LZ4 (via `lz4_flex`) for fast compression/decompression with reasonable ratios on memory dumps.

## Dependencies

- `windows` (0.58) — Win32 + Wdk bindings (Threading, Memory, Debug, ToolHelp, ProcessStatus, LibraryLoader)
- `serde` + `bincode` — Serialization of snapshot payloads
- `lz4_flex` (0.11) — LZ4 compression
- `anyhow` — Top-level error handling in `main`
- `thiserror` — Structured error enum (`QuickResumeError`)

## Workflow

### Snapshot
1. Find process by name (`CreateToolhelp32Snapshot` + `Process32First/Next`)
2. Open with `PROCESS_ALL_ACCESS`
3. `NtSuspendProcess` (atomic freeze of all threads)
4. `VirtualQueryEx` loop → `ReadProcessMemory` for each committed, non-skipped region
5. `GetThreadContext(CONTEXT_ALL)` for every thread
6. `NtResumeProcess`
7. Serialize with bincode → compress with LZ4 → write `.qrs` file with header

### Restore
1. Read and decompress `.qrs` file
2. `CreateProcessW(CREATE_SUSPENDED)` to launch the executable
3. Resume/suspend loop until OS loader maps ntdll + kernel32
4. For each saved region: `VirtualAllocEx` at exact base → `WriteProcessMemory` → `VirtualProtectEx`
5. For each saved thread: `SetThreadContext` on matching new thread (by index)
6. `ResumeThread` on main thread

## Current Status

**What's implemented:**
- Full snapshot pipeline (attach → suspend → memory dump → thread capture → serialize → compress → write)
- Full restore pipeline (read → decompress → launch suspended → inject memory → restore threads → resume)
- Process profiling/inspection command
- PE ASLR disable/restore utility
- Snapshot header peek command
- Structured error handling with typed error variants
- RAII guards for process suspension and handle cleanup

**Known limitations / TODO:**
- Thread matching by index assumes deterministic thread creation order — fragile for non-Unity targets
- No handle table snapshot (open file handles, sockets, etc. are lost on restore)
- No GPU/graphics state capture (DirectX/Vulkan resources are lost)
- No support for child processes or shared memory between processes
- Memory regions that fail `VirtualAllocEx` (already occupied by the new process) are skipped
- The `enumerate_modules` function in `info.rs` has a typo: references `snap` instead of `snapshot` (line ~146)
- No automated tests yet
- CLI is basic `env::args` parsing — no clap/structopt
- Snapshot format version is 1 with no migration path yet
- `std::mem::forget(suspended)` in restore leaks handles intentionally to keep the process alive

## Code Conventions

- Pure Rust, Windows-only (`windows` crate for all Win32/Wdk FFI)
- `unsafe` blocks are scoped tightly around Win32 calls with comments explaining safety
- Error types use `thiserror` derive; propagation via `?` and `anyhow` at the top level
- Module structure mirrors the three-phase pipeline: process → snapshot → restore
- Serde `Serialize`/`Deserialize` on all snapshot data structures
- Thread CONTEXT stored as raw bytes (`Vec<u8>`) to avoid serde issues with the large union type
- 16-byte aligned CONTEXT wrapper (`AlignedContext`) required by `GetThreadContext`/`SetThreadContext`
- Handle cleanup via `Drop` impls (`ProcessHandle`, `SuspendGuard`, `SuspendedProcess`)
