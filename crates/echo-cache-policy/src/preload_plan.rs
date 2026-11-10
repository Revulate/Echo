//! Pure preload planning helpers shared by runtime cache code and verification tools.

/// Number of channel emote URLs to preload eagerly before deferring the rest.
pub const EAGER_CHANNEL_PRELOAD_LIMIT: usize = 4;
/// Per-channel cap for startup/background image preloading.
pub const MAX_CHANNEL_PRELOAD_URLS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelPreloadPlan {
    pub discovered: usize,
    pub queued: usize,
    pub eager: usize,
    pub lazy: usize,
    pub truncated: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelPreloadSummary {
    pub channels: usize,
    pub discovered: usize,
    pub queued: usize,
    pub eager: usize,
    pub lazy: usize,
    pub truncated: usize,
}

pub fn plan_channel_preload(discovered: usize) -> ChannelPreloadPlan {
    let queued = discovered.min(MAX_CHANNEL_PRELOAD_URLS);
    let eager = queued.min(EAGER_CHANNEL_PRELOAD_LIMIT);
    let lazy = queued.saturating_sub(eager);
    let truncated = discovered.saturating_sub(queued);

    ChannelPreloadPlan {
        discovered,
        queued,
        eager,
        lazy,
        truncated,
    }
}

pub fn summarize_channel_preload_plans(
    plans: impl IntoIterator<Item = ChannelPreloadPlan>,
) -> ChannelPreloadSummary {
    let mut summary = ChannelPreloadSummary::default();
    for plan in plans {
        summary.channels = summary.channels.saturating_add(1);
        summary.discovered = summary.discovered.saturating_add(plan.discovered);
        summary.queued = summary.queued.saturating_add(plan.queued);
        summary.eager = summary.eager.saturating_add(plan.eager);
        summary.lazy = summary.lazy.saturating_add(plan.lazy);
        summary.truncated = summary.truncated.saturating_add(plan.truncated);
    }
    summary
}
