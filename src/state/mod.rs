//! State management for the application
//!
//! Uses GPUI's Entity system for reactive state management.
//!
//! ## Architecture
//!
//! - `AppState`: Application-level state (tabs, sidebars)
//! - `ChatBackend`: Per-channel chat state management
//! - `VersionRegistry`: Centralized version tracking for change detection

mod app_state;
mod chat_backend;
pub mod chat_observer;
pub(crate) mod chat_observer_recovery;
mod chat_state;
pub mod emote_preferences;
mod emote_state;
pub mod layout_eviction;
mod message_buffer;
pub mod message_retention;
mod split_pane;
mod tab_state;
pub mod ui_invalidation;
pub mod version_manager;
mod workspace_state;

/// Install the root lookups that the GPUI-free state crates cannot reach on
/// their own. Idempotent, and safe to call before the backend is initialized:
/// each port keeps its standalone default until this runs.
pub fn install_domain_ports() {
    echo_core::channel_identity::install_current_watching_channel(
        crate::backend::browser_watching::current_channel,
    );
    echo_core::install_log_category_enabled(crate::log::enabled);
    emote_preferences::install_ports();
    crate::emote_renderer::install_animation_clock_ports();
    echo_core::message_buffer::install_message_buffer_ports(
        echo_core::message_buffer::MessageBufferPorts {
            default_capacity: message_retention::active_channel_capacity,
            queue_evicted_id: layout_eviction::queue_evicted_id,
            queue_evicted_ids: |ids| layout_eviction::queue_evicted_ids(ids),
            prune_message_id: crate::backend::embeds::state::prune_message_id,
        },
    );
}

pub use app_state::{ActivitySection, AppState};
pub use chat_backend::{ChatBackend, ChatBackendEvent};
pub use chat_observer::ChatStateObserver;
pub use chat_state::{ChatState, ChatStateEvent, ReplyState};
pub use emote_state::{EmoteStateEvent, EmoteStateManager};
pub use message_buffer::{MessageBuffer, MessageReconcileAction};
pub use split_pane::{SplitId, SplitPane, SplitPaneEvent};
pub use tab_state::{TabHighlightState, TabState, TabVisibility};
pub use version_manager::VersionRegistry;
pub use workspace_state::{
    AddSplitDirection, DragDropState, DropZone, SplitNode, SplitOrientation, is_placeholder,
    new_placeholder,
};
