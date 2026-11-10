//! Opt-in measurement and self-test instrumentation: allocation and heap
//! profiling, tracing, the FPS profiler, startup markers and `--demo` mode.
//! Every module here is inert unless its environment switch, cargo feature or
//! command-line flag enables it.

pub mod alloc_profile;
pub mod demo;
pub mod dhat_profile;
pub mod fps_profiler;
pub mod profiling;
pub mod startup_metrics;
