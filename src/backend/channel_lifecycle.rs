//! Compatibility facade for channel lifecycle management.
//!
//! The lifecycle state machine is GPUI-free and lives in `echo-core`; the
//! process-global manager stays here so the whispers-channel predicate is
//! installed before any connection request reaches it.

use std::sync::LazyLock;

pub use echo_core::channel_lifecycle::*;

static CHANNEL_LIFECYCLE: LazyLock<ChannelLifecycleManager> = LazyLock::new(|| {
    install_whispers_channel_predicate(crate::backend::whispers::is_whispers_channel);
    ChannelLifecycleManager::new()
});

/// Get the global channel lifecycle manager
pub fn get_lifecycle_manager() -> &'static ChannelLifecycleManager {
    &CHANNEL_LIFECYCLE
}
