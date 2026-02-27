mod process;
mod restore;
mod snapshot;
mod util;

use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use process::{info, open_process_by_name, SuspendGuard};
use restore::{
    launch::launch_suspended,
    memory::restore_memory,
    reader::{peek_snapshot_header, read_snapshot},
    threads::restore_thread_contexts,
};
use snapshot::{
    memory::{dump_memory, print_stats},
    threads::capture_thread_contexts,
    writer::{build_payload, write_snapshot},
};
use util::pe;

// ── Default settings ──────────────────────────────────────────────────────────

const DEFAULT_TARGET: &str = "Mini Metro.exe";
const SNAPSHOT_DIR: &str = "snapshot";
const SNAPSHOT_FILE: &str = "mini_metro.qrs";

// ── CLI ───────────────────────────────────────────────────────────────────────

fn usage() {
    eprintln!(
        r#"Quick Resume — Xbox-style process snapshot/restore for Windows

USAGE:
  quick_resume --profile  [--target <exe>]       Enumerate process memory/threads
  quick_resume --snapshot [--target <exe>]       Capture full snapshot to disk
  quick_resume --restore  [--exe <path>]         Restore from snapshot
  quick_resume --disable-aslr <path>             Patch PE to disable ASLR
  quick_resume --restore-aslr <path>             Restore original PE from backup
  quick_resume --peek                            Print snapshot file header info

Options:
  --target <name>    Executable name to attach to (default: "Mini Metro.exe")
  --exe    <path>    Full path to executable for restore
  --out    <path>    Snapshot output path (default: snapshot/mini_metro.qrs)
"#
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        usage();
        std::process::exit(1);
    }

    let result = match args[1].as_str() {
        "--profile" => {
            let target = flag_value(&args, "--target").unwrap_or(DEFAULT_TARGET.to_string());
            cmd_profile(&target)
        }
        "--snapshot" => {
            let target = flag_value(&args, "--target").unwrap_or(DEFAULT_TARGET.to_string());
            let out = snapshot_path(&args);
            cmd_snapshot(&target, &out)
        }
        "--restore" => {
            let exe = flag_value(&args, "--exe");
            let snap = snapshot_path(&args);
            cmd_restore(exe.as_deref(), &snap)
        }
        "--disable-aslr" => {
            let path = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
                eprintln!("Usage: quick_resume --disable-aslr <path>");
                std::process::exit(1);
            });
            pe::disable_aslr(&path).map_err(Into::into)
        }
        "--restore-aslr" => {
            let path = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
                eprintln!("Usage: quick_resume --restore-aslr <path>");
                std::process::exit(1);
            });
            pe::restore_aslr(&path).map_err(Into::into)
        }
        "--peek" => {
            let snap = snapshot_path(&args);
            peek_snapshot_header(&snap).map_err(Into::into)
        }
        _ => {
            usage();
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

// ── Phase 1: Profile ──────────────────────────────────────────────────────────

fn cmd_profile(target: &str) -> anyhow::Result<()> {
    println!("[profile] Attaching to \"{}\" ...", target);
    let proc = open_process_by_name(target)?;
    let summary = info::build_summary(proc.handle, proc.pid, &proc.name)?;
    info::print_summary(&summary);
    Ok(())
}

// ── Phase 2: Snapshot ─────────────────────────────────────────────────────────

fn cmd_snapshot(target: &str, out: &Path) -> anyhow::Result<()> {
    println!("[snapshot] Attaching to \"{}\" ...", target);
    let proc = open_process_by_name(target)?;

    println!("[snapshot] Suspending process ...");
    let mut guard = SuspendGuard::suspend(proc.handle)?;

    let t0 = Instant::now();

    println!("[snapshot] Dumping memory regions ...");
    let regions = dump_memory(proc.handle)?;
    print_stats(&regions);

    println!("[snapshot] Capturing thread contexts ...");
    let threads = capture_thread_contexts(proc.pid)?;
    println!("[snapshot] Captured {} thread contexts.", threads.len());

    println!("[snapshot] Resuming process ...");
    guard.resume()?;

    println!("[snapshot] Serializing and compressing ...");
    let payload = build_payload(&proc.name, proc.pid, regions, threads);
    let (raw_bytes, compressed_bytes) = write_snapshot(&payload, out)?;

    let elapsed = t0.elapsed();
    println!(
        "[snapshot] Done in {:.2}s — {} regions, {} threads, {:.1} MB raw → {:.1} MB compressed ({:.1}x)",
        elapsed.as_secs_f64(),
        payload.memory_regions.len(),
        payload.thread_snapshots.len(),
        raw_bytes as f64 / 1_048_576.0,
        compressed_bytes as f64 / 1_048_576.0,
        raw_bytes as f64 / compressed_bytes.max(1) as f64,
    );
    println!("[snapshot] Written to: {}", out.display());
    Ok(())
}

// ── Phase 3: Restore ──────────────────────────────────────────────────────────

fn cmd_restore(exe_path: Option<&str>, snap_path: &Path) -> anyhow::Result<()> {
    println!("[restore] Reading snapshot: {}", snap_path.display());
    let payload = read_snapshot(snap_path)?;

    println!(
        "[restore] Snapshot: {} (PID {}), {} regions, {} threads",
        payload.process_name,
        payload.pid,
        payload.memory_regions.len(),
        payload.thread_snapshots.len(),
    );

    let exe = exe_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&payload.process_name));

    if !exe.exists() {
        return Err(anyhow::anyhow!(
            "Executable not found: {}\nProvide --exe <full path> to the game.",
            exe.display()
        ));
    }

    println!("[restore] Launching suspended: {}", exe.display());
    let t0 = Instant::now();

    let suspended = launch_suspended(&exe)?;

    println!("[restore] Restoring memory ({} regions) ...", payload.memory_regions.len());
    restore_memory(suspended.process_handle, &payload.memory_regions)?;

    println!("[restore] Restoring thread contexts ...");
    restore_thread_contexts(suspended.pid, &payload.thread_snapshots)?;

    println!("[restore] Resuming process ...");
    suspended.resume_main_thread()?;

    let elapsed = t0.elapsed();
    println!(
        "[restore] Done in {:.2}s — PID {} is running.",
        elapsed.as_secs_f64(),
        suspended.pid
    );

    // Keep handles alive a moment so the process can stabilize.
    std::mem::forget(suspended); // Drop will close handles, but process stays alive.
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn snapshot_path(args: &[String]) -> PathBuf {
    flag_value(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(SNAPSHOT_DIR).join(SNAPSHOT_FILE))
}
