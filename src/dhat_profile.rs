//! Heap profiling with call-stack attribution, behind the `dhat-heap` feature.
//!
//! Aggregate counters can say *how much* is live; only a profiler can say *which
//! call sites own it*. This wires up dhat, which records a backtrace per
//! allocation and writes a JSON file listing, per program point, the bytes live
//! at peak and at end.
//!
//! Build and run:
//! ```text
//! cargo build --features dhat-heap --bin echo
//! ECHO_DHAT_SECS=90 ECHO_DHAT_FILE=dhat-heap.json ./echo
//! ```
//!
//! The profile is written when the profiler is dropped. GPUI's run loop does not
//! reliably return on Windows, so rather than relying on process exit this drops
//! the profiler from a timer thread after `ECHO_DHAT_SECS`, which flushes the
//! file while the app keeps running.
//!
//! This replaces mimalloc with dhat's tracking allocator: expect a large
//! slowdown and different absolute numbers. Attribution is the point, not
//! magnitude.
//!
//! The `dhat-heap` feature also suppresses `windows_subsystem = "windows"` in
//! main.rs. dhat prints its summary to stderr before saving, so in a
//! GUI-subsystem process that `eprintln!` panics on the invalid stderr handle
//! and the dump thread dies without ever writing the file -- silently, since
//! there is no console to see the panic on.

#[cfg(feature = "dhat-heap")]
static PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

/// Begin heap profiling if the feature is compiled in. No-op otherwise.
#[cfg(feature = "dhat-heap")]
pub fn start() {
    let file = if crate::benchmark_profile::is_enabled() {
        crate::benchmark_profile::profiling_artifact_path("dhat-heap.json")
            .display()
            .to_string()
    } else {
        std::env::var("ECHO_DHAT_FILE").unwrap_or_else(|_| "dhat-heap.json".to_string())
    };
    let secs = std::env::var("ECHO_DHAT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);

    let profiler = dhat::Profiler::builder().file_name(&file).build();
    *PROFILER.lock().unwrap() = Some(profiler);
    tracing::info!(file = %file, secs, "dhat heap profiling started");

    std::thread::Builder::new()
        .name("dhat-dump".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let taken = PROFILER.lock().unwrap().take();
            drop(taken);
            tracing::info!(file = %file, "dhat heap profile written");
        })
        .ok();
}

#[cfg(not(feature = "dhat-heap"))]
pub fn start() {}
