//! Runaway-memory guard: a tracking global allocator plus a watchdog thread
//! that panics the process and writes a memory dump once live allocations cross
//! a configurable ceiling (default 10 GiB).
//!
//! ## Why
//!
//! A raw parse loop over a large corpus (or a pathological single unit) can grow
//! the process-global interner / a runaway AST without bound and take the whole
//! machine down via OOM before anything reports. This guard is the last-resort
//! backstop: it stops the process at a known ceiling and leaves a dump so the
//! allocation can be diagnosed after the fact, instead of a hard machine crash.
//!
//! ## Design (re-entrancy safety)
//!
//! The allocator hot path does NOTHING but update two atomic counters — no
//! branching into dump/format code that would re-enter the allocator. A separate
//! [`install`]ed **watchdog thread** polls the counter and, on the first
//! crossing, does the heavy work (write the dump, abort) from a normal stack
//! where allocation is safe. That up-to-`poll_interval` latency is irrelevant
//! against a multi-GiB ceiling and buys full re-entrancy safety.
//!
//! ## Wiring
//!
//! The [`TrackingAllocator`] only tracks when it is the process
//! `#[global_allocator]`, which must be declared in the *binary* crate:
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: delphi_parser::mem_guard::TrackingAllocator =
//!     delphi_parser::mem_guard::TrackingAllocator;
//!
//! fn main() {
//!     delphi_parser::mem_guard::install(delphi_parser::mem_guard::GuardConfig::default());
//!     // ...
//! }
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Default ceiling: 10 GiB of live allocation.
pub const DEFAULT_THRESHOLD_BYTES: usize = 10 * 1024 * 1024 * 1024;

/// Environment override, in whole gibibytes (`DDK_MEM_LIMIT_GB=6`). `0` disables
/// the guard entirely (the watchdog still installs but never trips).
pub const THRESHOLD_ENV: &str = "DDK_MEM_LIMIT_GB";

/// Live bytes currently handed out by the tracking allocator (approximate: it
/// counts the `Layout` size of every alloc/realloc, which is the requested size,
/// not the allocator's rounded-up block — close enough for a GiB-scale ceiling).
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of [`LIVE_BYTES`] — reported in the dump so a run that came
/// close without tripping is still visible.
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Set once when the watchdog fires, so the dump/abort happens exactly once.
static TRIPPED: AtomicBool = AtomicBool::new(false);
/// Set once when [`install`] has spawned the watchdog, so a second call is a
/// no-op instead of a second thread.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Current live allocation in bytes (tracked-allocator view).
pub fn current_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Peak live allocation in bytes since process start.
pub fn peak_bytes() -> usize {
    PEAK_BYTES.load(Ordering::Relaxed)
}

/// The pure trip predicate — separated so it is unit-testable without a real
/// allocation. A threshold of `0` disables tripping.
pub fn should_trip(current: usize, threshold: usize) -> bool {
    threshold != 0 && current > threshold
}

#[inline]
fn record_alloc(size: usize) {
    let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
    // Monotonically raise the peak. A relaxed CAS loop is fine: an occasional
    // lost race under-reports the peak by one concurrent delta, never over-.
    let mut peak = PEAK_BYTES.load(Ordering::Relaxed);
    while live > peak {
        match PEAK_BYTES.compare_exchange_weak(
            peak,
            live,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }
}

#[inline]
fn record_dealloc(size: usize) {
    LIVE_BYTES.fetch_sub(size, Ordering::Relaxed);
}

/// Test-only: force the live-byte counter to a value, so the watchdog can be
/// exercised WITHOUT this being the process `#[global_allocator]` (in a plain
/// test binary the tracking allocator is not installed, so real allocations are
/// invisible to the counter).
#[cfg(test)]
pub fn force_live_bytes_for_test(bytes: usize) {
    LIVE_BYTES.store(bytes, Ordering::SeqCst);
}

/// A `System`-delegating global allocator that tracks live bytes. The hot path
/// is only the delegation plus an atomic add/sub — deliberately no threshold
/// check here (the watchdog owns that), so `alloc`/`dealloc` never branch into
/// code that could re-enter the allocator.
pub struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        record_dealloc(layout.size());
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // Adjust by the delta: the old block is gone, the new block is live.
            record_dealloc(layout.size());
            record_alloc(new_size);
        }
        new_ptr
    }
}

/// Watchdog configuration.
#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Ceiling in bytes; crossing it trips the guard. `0` disables tripping.
    pub threshold_bytes: usize,
    /// Directory the dump + report are written to. Created if missing.
    pub dump_directory: PathBuf,
    /// How often the watchdog samples the live-byte counter.
    pub poll_interval: Duration,
    /// Write a full-memory Windows minidump (the whole heap — large, up to the
    /// ceiling on disk). When false, only the small text report is written.
    pub full_memory_dump: bool,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            threshold_bytes: threshold_from_env().unwrap_or(DEFAULT_THRESHOLD_BYTES),
            dump_directory: std::env::temp_dir().join("ddk-dumps"),
            poll_interval: Duration::from_millis(250),
            full_memory_dump: true,
        }
    }
}

/// Read `DDK_MEM_LIMIT_GB` (whole GiB). Absent/unparseable → `None` (caller uses
/// the default); an explicit `0` → `Some(0)` which disables tripping.
fn threshold_from_env() -> Option<usize> {
    let raw = std::env::var(THRESHOLD_ENV).ok()?;
    let gib: usize = raw.trim().parse().ok()?;
    Some(gib.saturating_mul(1024 * 1024 * 1024))
}

/// Install the watchdog thread. Idempotent: a second call is a no-op. Safe to
/// call before or after the first allocations. Does nothing observable until the
/// [`TrackingAllocator`] is the process `#[global_allocator]` (otherwise the
/// counter stays at zero and the guard never trips).
pub fn install(config: GuardConfig) {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let threshold = config.threshold_bytes;
    eprintln!(
        "[ddk mem-guard] armed: limit {} ({} bytes), poll {:?}, dumps -> {}",
        human_bytes(threshold),
        threshold,
        config.poll_interval,
        config.dump_directory.display(),
    );
    std::thread::Builder::new()
        .name("ddk-mem-guard".into())
        .spawn(move || watchdog_loop(config))
        .expect("mem-guard watchdog thread must spawn");
}

fn watchdog_loop(config: GuardConfig) {
    loop {
        std::thread::sleep(config.poll_interval);
        let live = current_bytes();
        if should_trip(live, config.threshold_bytes)
            && !TRIPPED.swap(true, Ordering::SeqCst)
        {
            handle_exceeded(live, &config);
        }
    }
}

/// The trip action: announce, write the dump + report, then abort the process.
/// Aborts (not a mere thread panic) because the runaway is on ANOTHER thread —
/// only killing the whole process actually stops the memory growth. A panic on
/// the watchdog thread alone would leave the offender running.
fn handle_exceeded(live: usize, config: &GuardConfig) {
    eprintln!(
        "\n[ddk mem-guard] LIMIT EXCEEDED: live {} > limit {} (peak {}). \
         Writing dump and aborting.",
        human_bytes(live),
        human_bytes(config.threshold_bytes),
        human_bytes(peak_bytes()),
    );

    match write_dump(&config.dump_directory, live, config.full_memory_dump) {
        Ok(paths) => {
            for path in paths {
                eprintln!("[ddk mem-guard] wrote {}", path.display());
            }
        }
        Err(error) => {
            eprintln!("[ddk mem-guard] dump FAILED: {error}");
        }
    }

    eprintln!("[ddk mem-guard] aborting now.");
    std::process::abort();
}

/// Write the diagnostic artifacts for an out-of-memory trip into `directory`:
/// always a small `*.txt` report (live/peak bytes), and on Windows a
/// `*.dmp` process dump (full-memory when `full_memory` is set). Returns the
/// paths written.
///
/// Public + `full_memory=false` for tests (the text report is cheap and
/// portable; the real minidump is exercised by an ignored Windows test).
pub fn write_dump(
    directory: &Path,
    live_bytes: usize,
    full_memory: bool,
) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(directory)?;
    let process_id = std::process::id();
    let stem = format!("ddk-oom-pid{process_id}");
    let mut written = Vec::new();

    // Text report first — cheapest, and useful even if the dump write fails.
    let report_path = directory.join(format!("{stem}.txt"));
    let report = format!(
        "delphi-devkit out-of-memory guard report\n\
         process id     : {process_id}\n\
         live bytes     : {live_bytes} ({live_human})\n\
         peak bytes     : {peak} ({peak_human})\n",
        live_human = human_bytes(live_bytes),
        peak = peak_bytes(),
        peak_human = human_bytes(peak_bytes()),
    );
    std::fs::write(&report_path, report)?;
    written.push(report_path);

    #[cfg(windows)]
    {
        let dump_path = directory.join(format!("{stem}.dmp"));
        write_windows_minidump(&dump_path, full_memory)?;
        written.push(dump_path);
    }
    #[cfg(not(windows))]
    {
        let _ = full_memory; // no OS minidump facility here
    }

    Ok(written)
}

#[cfg(windows)]
fn write_windows_minidump(path: &Path, full_memory: bool) -> std::io::Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
    };
    use windows::Win32::System::Diagnostics::Debug::{
        MiniDumpNormal, MiniDumpWithFullMemory, MiniDumpWithHandleData, MiniDumpWriteDump,
        MINIDUMP_TYPE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
    use windows::core::PCWSTR;

    // UTF-16, NUL-terminated path for the wide Win32 file API.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let dump_type: MINIDUMP_TYPE = if full_memory {
        MINIDUMP_TYPE(MiniDumpWithFullMemory.0 | MiniDumpWithHandleData.0)
    } else {
        MiniDumpNormal
    };

    unsafe {
        let file = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(|error| std::io::Error::other(format!("CreateFileW failed: {error}")))?;

        let result = MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file,
            dump_type,
            None,
            None,
            None,
        );

        let _ = CloseHandle(file);
        result.map_err(|error| std::io::Error::other(format!("MiniDumpWriteDump failed: {error}")))?;
    }
    Ok(())
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

/// Human-readable byte size (binary units), for log/report lines only.
fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_trip_respects_threshold_and_disable() {
        assert!(should_trip(11, 10));
        assert!(!should_trip(10, 10)); // strictly greater
        assert!(!should_trip(9, 10));
        // threshold 0 disables the guard entirely
        assert!(!should_trip(usize::MAX, 0));
    }

    #[test]
    fn record_alloc_dealloc_track_live_and_peak() {
        let base = current_bytes();
        let peak_base = peak_bytes();
        record_alloc(1000);
        assert_eq!(current_bytes(), base + 1000);
        assert!(peak_bytes() >= peak_base + 1000);
        let after_peak = peak_bytes();
        record_dealloc(1000);
        assert_eq!(current_bytes(), base);
        // peak never decreases on dealloc
        assert_eq!(peak_bytes(), after_peak);
    }

    #[test]
    fn human_bytes_formats_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(10 * 1024 * 1024 * 1024), "10.00 GiB");
    }

    #[test]
    fn threshold_default_is_ten_gib() {
        assert_eq!(DEFAULT_THRESHOLD_BYTES, 10 * 1024 * 1024 * 1024);
    }

    /// End-to-end: arming the watchdog with a low ceiling and pushing the live
    /// counter past it must (a) write a dump + report and (b) ABORT the process.
    /// Abort would kill the test runner, so this re-execs THIS test in a child
    /// process (fresh globals, no parallel-test races) and asserts from the
    /// parent that the child died and left a dump behind.
    #[test]
    fn watchdog_dumps_and_aborts_when_exceeded() {
        // CHILD role: arm a tiny guard, exceed it, then wait to be aborted.
        if let Ok(dir) = std::env::var("DDK_MEMGUARD_CHILD_DIR") {
            install(GuardConfig {
                threshold_bytes: 1024,
                dump_directory: std::path::PathBuf::from(dir),
                poll_interval: Duration::from_millis(10),
                full_memory_dump: false,
            });
            force_live_bytes_for_test(64 * 1024 * 1024);
            // The watchdog should abort us well within this window. If we ever
            // return, the guard failed to fire and the child exits 0 (parent
            // treats a clean exit as failure).
            std::thread::sleep(Duration::from_secs(10));
            std::process::exit(0);
        }

        // PARENT role: re-exec this exact test in a child with the trigger env.
        let dir = std::env::temp_dir().join(format!("ddk-memguard-child-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = std::env::current_exe().expect("test exe path");
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                "mem_guard::tests::watchdog_dumps_and_aborts_when_exceeded",
                "--nocapture",
            ])
            .env("DDK_MEMGUARD_CHILD_DIR", &dir)
            .status()
            .expect("spawn child");

        // A guard abort is a non-success exit (Windows abort → code 3); a clean
        // exit 0 means the watchdog never fired.
        assert!(
            !status.success(),
            "child exited cleanly — the watchdog did not abort on the exceeded limit"
        );
        // And it must have left a dump behind.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(
            entries.iter().any(|p| p.extension().map(|e| e == "txt").unwrap_or(false)),
            "expected an out-of-memory report in {}; found {entries:?}",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_dump_writes_report_and_minidump() {
        let dir = std::env::temp_dir().join("ddk-mem-guard-test");
        // full_memory=false → a small `MiniDumpNormal` (stacks only), cheap even
        // in a test. On Windows this exercises the real `MiniDumpWriteDump` FFI:
        // a failure returns Err, so reaching the asserts proves the call worked.
        let paths = write_dump(&dir, 123_456, false).expect("dump write");

        let report = paths
            .iter()
            .find(|p| p.extension().map(|e| e == "txt").unwrap_or(false))
            .expect("a .txt report");
        let body = std::fs::read_to_string(report).expect("read report");
        assert!(body.contains("123456"));
        assert!(body.contains("out-of-memory guard report"));
        let _ = std::fs::remove_file(report);

        #[cfg(windows)]
        {
            let dump = paths
                .iter()
                .find(|p| p.extension().map(|e| e == "dmp").unwrap_or(false))
                .expect("a .dmp minidump on Windows");
            let size = std::fs::metadata(dump).expect("dump metadata").len();
            assert!(size > 0, "minidump must be non-empty");
            let _ = std::fs::remove_file(dump);
        }
    }
}
