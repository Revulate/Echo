//! GPUI-free cache policy shared by Echo and verification tools.

pub mod budget;
pub mod disk_model;
pub mod eviction;
pub mod loaded_urls;
pub mod preload_plan;
pub mod stats;

pub use budget::{BudgetManager, CacheLayer};
pub use eviction::{AccessBuffer, Evictable, EvictableEntry, EvictionPolicy};
pub use preload_plan::{
    ChannelPreloadPlan, ChannelPreloadSummary, EAGER_CHANNEL_PRELOAD_LIMIT,
    MAX_CHANNEL_PRELOAD_URLS, plan_channel_preload, summarize_channel_preload_plans,
};
pub use stats::{CacheStats, DiagnosticStatsSnapshot, StatsSnapshot};
