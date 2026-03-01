# quick-resume

An experimental Xbox-style Quick Resume for Windows. Snapshots a running process's full state — virtual memory and thread register contexts — to a compressed file on disk, then restores that exact state into a freshly spawned instance of the same executable later.

The goal is to freeze a game mid-session, shut down the machine or move on to something else, and resume from that exact point without sitting through loading screens or restarting a run.

This project is experimental. It works in narrow conditions and has several fundamental limitations described below.

---

## How it works

### Snapshot

1. Finds the target process by name using the Windows Toolhelp API.
2. Atomically freezes all threads with `NtSuspendProcess`.
3. Walks the virtual address space with `VirtualQueryEx` and reads every committed, writable, non-guard region with `ReadProcessMemory`.
4. Captures the full register context (`CONTEXT_ALL`) for every thread with `GetThreadContext`.
5. Resumes the process, then serializes the captured data with bincode and compresses it with LZ4.

### Restore

1. Reads and decompresses the snapshot file.
2. Launches the target executable with `CreateProcessW(CREATE_SUSPENDED)`.
3. Waits for the OS loader to map the system DLLs (`ntdll.dll`, `kernel32.dll` / `wow64cpu.dll`) before touching memory.
4. Allocates and writes each saved memory region into the new process at its original address using `VirtualAllocEx` + `WriteProcessMemory`. For regions already mapped (DLL pages), falls back to `VirtualProtectEx` + `WriteProcessMemory` in place.
5. Restores thread register contexts with `SetThreadContext`, matched by creation-order index.
6. Resumes the main thread.

### ASLR

For addresses to match between the snapshot and restore sessions, the target executable must load at its preferred base address. The `--disable-aslr` command patches the PE header on disk to clear the `IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE` flag. A `.bak` backup is always created first. DLLs loaded by the process are not patched, which means their addresses may differ across runs — this is a known limitation.

---

## Snapshot file format (`.qrs`)

```
[4 bytes]  Magic: "QRSV"
[4 bytes]  Version: u32 little-endian (currently 1)
[8 bytes]  Timestamp: u64 unix milliseconds little-endian
[8 bytes]  Uncompressed payload size: u64 little-endian
[N bytes]  LZ4-compressed bincode payload (SnapshotPayload)
```

`SnapshotPayload` contains the process name, PID, WOW64 flag, a list of `MemoryRegion` structs (base address, size, protection flags, type, raw bytes), and a list of `ThreadSnapshot` structs (TID, raw `CONTEXT` bytes).

---

## Usage

Build with `cargo build --release`. All commands are run from the project root or the `target/release/` directory.

```
# Inspect a running process (memory regions, threads, loaded modules)
quick_resume.exe --profile [--target <exe_name>]

# Snapshot a running process
quick_resume.exe --snapshot [--target <exe_name>] [--out <path.qrs>]

# Restore from a snapshot
quick_resume.exe --restore --exe <path_to_executable> [--out <path.qrs>]

# Patch the target executable to disable ASLR (creates .bak backup)
quick_resume.exe --disable-aslr <path_to_executable>

# Restore the original executable from backup
quick_resume.exe --restore-aslr <path_to_executable>

# Print snapshot file header without deserializing the full payload
quick_resume.exe --peek [--out <path.qrs>]
```

Default target: `Mini Metro.exe`. Default snapshot path: `snapshot/mini_metro.qrs`.

### Typical workflow

```
# One-time setup: disable ASLR on the target executable
quick_resume.exe --disable-aslr "C:\Path\To\Game\Game.exe"

# While the game is running at a point you want to save:
quick_resume.exe --snapshot --target "Game.exe" --out snapshot\game.qrs

# Later, to resume:
quick_resume.exe --restore --exe "C:\Path\To\Game\Game.exe" --out snapshot\game.qrs
```

---

## Dependencies

| Crate | Purpose |
|---|---|
| `windows` 0.58 | Win32 / Wdk bindings for all system calls |
| `serde` + `bincode` | Serialization of snapshot payloads |
| `lz4_flex` 0.11 | LZ4 compression / decompression |
| `anyhow` | Top-level error handling in main |
| `thiserror` | Structured `QuickResumeError` enum |

---

## Current status

### What works

- Full snapshot pipeline: attach, suspend, memory dump, thread capture, serialize, compress, write.
- Full restore pipeline: read, decompress, launch suspended, wait for loader, inject memory, restore thread contexts, resume.
- Memory region filtering: skips `PAGE_NOACCESS`, `PAGE_GUARD` (stack expansion pages), and read-only `MEM_IMAGE` pages (PE sections reloaded by the OS loader). This reduces snapshot size by roughly 30-50% and eliminates spurious read errors.
- Fallback write path: when `VirtualAllocEx` fails for a region already mapped by a DLL, the code tries `VirtualProtectEx` + `WriteProcessMemory` directly on the existing mapping.
- WOW64 detection: the snapshot records whether the target was a 32-bit process. Thread contexts are always captured and restored as 64-bit `CONTEXT` structs, since WOW64 threads execute through the 64-bit `wow64cpu.dll` translation layer.
- ASLR disable/restore utility.
- Graceful degradation: individual region or thread failures are logged and counted rather than aborting the restore. The restore only fails hard if zero regions or zero thread contexts could be written.
- `SuspendGuard` RAII: `NtResumeProcess` is always called on drop, even on panic, so the target process is never left permanently frozen.
- Snapshot header peek command.

### What has been tested

- Celeste (32-bit WOW64, MonoGame) — restore completes, process launches. Full game-state recovery is not yet confirmed due to DLL ASLR.
- Mini Metro (32-bit WOW64, Unity) — restore completes, process launches, Unity crash handler appears. Thread instruction pointers likely point into ASLR-shifted DLL addresses.
- Kingdom Come Deliverance 1 - snapshots successfully, but upon restoration `Error: LZ4 decompress failed: provided output is too small for the decompressed data, actual 816474308, expected 816474332`

---

## Known limitations and remaining work

**DLL ASLR** is the primary blocker for reliable game-state recovery. The executable's ASLR is disabled, but loaded DLLs (engine runtimes, audio libraries, Steam overlay, etc.) are still randomized each run. Thread instruction pointers saved in the snapshot point into DLL code at the snapshot-time addresses. After restore, those addresses belong to different mappings in the new process. Fixing this requires either disabling ASLR on all loaded DLLs before snapshotting, or relocating the saved thread contexts to the new DLL base addresses.

**No handle table snapshot.** Open file handles, sockets, DirectX device handles, and any other kernel object handles are not captured. The restored process will be missing these and will likely crash or malfunction when it tries to use them.

**No GPU / graphics state.** DirectX and Vulkan resources (swap chains, command queues, textures, buffers) live in the GPU driver and cannot be captured with `ReadProcessMemory`. Restoring a game that has initialised a graphics API will crash when it next touches the device.

**Thread matching by index.** Threads in the restored process are matched to saved contexts by creation order, not by TID. This is fragile for processes that create threads non-deterministically at startup.

**No child process support.** If the target spawns child processes (launchers, crash reporters, etc.), those are not captured.

**No snapshot migration.** The file format is version 1 with no upgrade path. A snapshot taken with an older build may not be readable by a newer one if the `SnapshotPayload` schema changes.

**No automated tests.** The codebase has no unit or integration tests. All validation is manual.

**Basic CLI.** Argument parsing uses `std::env::args` directly rather than a library like `clap`.
