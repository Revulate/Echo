//! GPUI-free rendering data structures shared by Echo and verification tools.

pub mod animation_clock;
pub mod animation_timing;
pub mod bidi;
pub mod design_tokens;
pub mod eviction_cache;
pub mod height_tree;
pub mod snapshot_membership;
pub mod spacing;

pub use bidi::{BidiAnalysis, contains_rtl};
pub use eviction_cache::EvictionTrackingCache;
pub use height_tree::HeightTree;
pub use spacing::{BoundarySpacingState, PriorContentKind};
