//! Echo GPUI library facade for integration benches and external tooling.
//!
//! The main application entrypoint remains `src/main.rs`.

#![recursion_limit = "4096"]

pub mod app;
pub mod assets;
pub mod backend;
pub mod bench;
pub mod cache;
pub mod components;
pub mod config;
pub mod diagnostics;
pub mod emote_renderer;
pub mod event_handlers;
pub mod icons;
pub mod identity;
pub mod image_decode;
pub mod log;
pub mod managers;
pub mod state;
pub mod storage_roots;
pub mod theme;
pub mod utils;

pub use app::EchoApp;
