//! App-side media cache: loading, decoding and residency of emote, badge and
//! link images.
//!
//! `loader` schedules downloads, disk hydration, decoding and preloads, and
//! is what the UI calls. The storage layers themselves (memory, frames, disk)
//! and the picker-visibility and deferred image-release registries live in
//! the GPUI-light `echo-media-cache` crate; `budget`, `disk_layer` and
//! `frame_layer` expose them under `crate::cache`. `ImageCacheService` owns
//! startup and shutdown of the cache runtime.

mod animation_source;
pub mod budget;
pub mod disk_layer;
pub mod frame_layer;
pub mod loader;
pub mod native_sizes;
pub mod service;
pub mod stats;
pub mod uri_layer;
pub mod warming;

pub use echo_media_cache::registry::*;
pub use service::ImageCacheService;
