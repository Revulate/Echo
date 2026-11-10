//! Cache subsystem modules.
//!
//! The active runtime cache path is owned by `ImageCacheService`.
//! `cache::loader` remains as a compatibility facade for hot-path callers.
//!
//! The picker-visibility and deferred image-release registries are cache
//! runtime state and live in `echo-media-cache`; they are re-exported here so
//! existing `crate::cache::...` callers are unchanged.

mod animation_source;
pub mod budget;
pub mod disk_layer;
pub mod frame_layer;
pub mod loader;
pub mod memory_layer;
pub mod native_sizes;
pub mod preload_plan;
pub mod service;
pub mod stats;
pub mod uri_layer;
pub mod warming;

pub use echo_media_cache::registry::*;
pub use service::ImageCacheService;
