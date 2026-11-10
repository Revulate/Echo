# Memory profiling

Tools for finding out how much memory Echo holds and which code owns it. Run
them in an isolated profile (`ECHO_BENCH_PROFILE_ROOT`) with a local chat replay
so runs are repeatable; see `docs/BENCHMARKING.md` for both, and
`docs/ENVIRONMENT.md` for every variable.

## Allocators

Windows builds use the Windows heap, and Echo's manifest opts into the segment
heap. Build with `--features windows-mimalloc` to use mimalloc instead. Linux
always uses mimalloc. Under mimalloc, Echo sets
`MIMALLOC_PAGE_COMMIT_ON_DEMAND=1` at startup unless it is already set, and
`ECHO_MIMALLOC_TRIM_INTERVAL_SECS=N` runs `mi_collect` every N seconds.

Allocator choice changes the private-byte and working-set numbers, so only
compare runs built with the same allocator.

## Memory breakdown log

`ECHO_MEM_REPORT_SECS=N` logs one "memory breakdown" line every N seconds to the
`perf` category. Pair it with `ECHO_LOG_FILE=1 ECHO_LOG=perf`:

```powershell
$env:ECHO_BENCH_PROFILE_ROOT = "$env:TEMP\echo-mem"
$env:ECHO_LOG_FILE = "1"
$env:ECHO_LOG = "perf"
$env:ECHO_MEM_REPORT_SECS = "10"
target\dist\echo.exe
```

The line sets what the process holds (`ws_mb`, `private_mb`, allocator commit and
peaks) against Echo's own caches: decoded static images and animation frames,
animation sources in RAM and on disk, the hot-frame and RenderImage queues, 7TV
name-paint composites, the GPUI URI image cache, the layout cache (an estimate
per entry), buffered chat rows and the emote maps. `accounted_mb` sums decoded
media, the layout-cache estimate, buffered chat rows, the emote maps and
name-paint composites; `unaccounted_mb` is the working set minus that sum. A
large or growing `unaccounted_mb` means memory that none of Echo's budgets
track. The implementation is
`src/backend/memory_report.rs`.

`run-perf-rig.ps1` sets `ECHO_MEM_REPORT_SECS` and summarizes these lines per
run.

## Heap attribution with dhat

The `dhat-heap` feature replaces the allocator with dhat's tracking allocator,
which records a backtrace per allocation and writes a JSON profile listing the
bytes live at peak and at the end for every allocation site:

```sh
cargo build --features dhat-heap --bin echo
ECHO_DHAT_SECS=90 ECHO_DHAT_FILE=dhat-heap.json ./target/debug/echo
```

- `ECHO_DHAT_SECS` (default 90) sets when the profile is written. GPUI's run
  loop does not reliably return on Windows, so a timer thread drops the
  profiler after that many seconds and Echo keeps running.
- `ECHO_DHAT_FILE` sets the output file (default `dhat-heap.json` in the working
  directory). Under `ECHO_BENCH_PROFILE_ROOT` the file always goes to
  `<profile>/logs/dhat-heap.json` and `ECHO_DHAT_FILE` is ignored.
- Open the file in dhat's viewer (`dh_view.html` from Valgrind, or the online
  copy).
- Expect a large slowdown and different absolute numbers. Use it for
  attribution, not for magnitude.
- The feature keeps the console subsystem on Windows, because dhat prints its
  summary to stderr before saving and a GUI-subsystem process has no valid
  stderr.

## Per-frame allocation counts

Build with `--features alloc-profile` and set `ECHO_ALLOC_PROFILE=1` to count
allocations, deallocations and reallocations per rendered frame. The FPS
overlay (`ECHO_FPS_OVERLAY=1`) shows the last frame's counts. Without the
feature, `ECHO_ALLOC_PROFILE` is ignored with a warning.

## Process-level tools

| Tool | Use |
| --- | --- |
| `scripts/bench/sample-process.ps1` / `.py` | Working set, private bytes, paged and virtual memory, handles and threads at a fixed interval. |
| `scripts/bench/capture-process-memory-map.ps1` | One virtual-memory map of a running process, with the largest allocations listed. |
| `scripts/bench/capture-windows-gpu-counters.ps1` | Per-process GPU memory counters and process samples over a long run, with optional periodic memory maps. |
| `scripts/bench/summarize-long-session-samples.*` | Growth over a long session from saved samples. |
| `scripts/bench/summarize-cleanup-samples.*` | Memory before and after leaving channels, from saved samples. |
| `scripts/bench/run-lifecycle-churn.ps1` | Repeated split, popout and scroll cycles, then checks for growing handles, threads or private memory. |
| `make heap` (Linux) | heaptrack through `scripts/profile.sh heap`. `make memcheck` runs Valgrind memcheck. |

## Provenance

`scripts/bench/record-performance-provenance.ps1` ties a profiling run's inputs
together after the CPU and GPU collectors have written their evidence. It does
not launch Echo, clear caches or change a cache tree.

```powershell
pwsh -NoProfile -File scripts/bench/record-performance-provenance.ps1 `
  -RunDir <run-dir> `
  -EchoExecutable target\dist\echo.exe `
  -CacheTree 'echo-image-cache=<path-to-cache>' `
  -CpuEvidencePath <run-dir>\cpu-sampling.json `
  -CpuLaunchPath target\dist\echo.exe `
  -GpuEvidencePath <run-dir>\gpu-presentmon-capture.json `
  -GpuLaunchPath target\dist\echo.exe
```

`-CacheTree` accepts one or more `label=directory` entries. For each tree the
script hashes every regular file with SHA-256 and writes one root SHA-256 over
the sorted relative path, byte count and per-file hash records, which
identifies both the cache contents and its layout without copying cache data.
The resulting `performance-provenance.json` also hashes the executable and both
evidence files, and requires each recorded launch executable to match the
profiled executable.
