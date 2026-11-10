//! Echo GPUI library facade for integration benches and external tooling.
//!
//! The main application entrypoint remains `src/main.rs`.

#![recursion_limit = "4096"]

pub mod alloc_profile;
pub mod app;
pub mod assets;
pub mod backend;
pub mod benchmark_profile;
pub mod benchmark_ui_fixture;
pub mod cache;
pub mod components;
pub mod config;
pub mod demo;
pub mod dhat_profile;
pub mod emote_renderer;
pub mod event_handlers;
pub mod fps_profiler;
pub mod icons;
pub mod identity;
pub mod image_decode;
pub mod log;
pub mod managers;
pub mod profiling;
pub mod startup_metrics;
pub mod state;
pub mod theme;
pub mod utils;

pub use app::EchoApp;
