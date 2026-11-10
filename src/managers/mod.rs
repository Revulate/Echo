//! App-shell coordination that is not itself a view.
//!
//! Three shapes live here:
//!
//! - GPUI entities owned by `EchoApp`: **OverlayManager** (modal lifecycle,
//!   positioning, z-ordering), **SplitLayoutManager** (active split, drag-drop,
//!   resize, bounds tracking) and **PollingCoordinator** (the background polling
//!   loop and UI-action dispatch).
//! - Plain state owned by a component: **TabManager** (inside the tab bar
//!   renderer: drag reorder, overflow, context menu) and **InputManager**
//!   (inside the chat input renderer: per-split drafts and input history).
//! - Free-function modules: **frame_rate_controller** (render mode and wakeups),
//!   **dynamic_tick_scheduler** (animation tick cadence) and **render_hooks**
//!   (side effects queued from render).

pub mod dynamic_tick_scheduler;
pub mod frame_rate_controller;
pub mod input_manager;
pub mod overlay_manager;
pub mod polling_coordinator;
pub mod render_hooks;
pub mod split_layout_manager;
pub mod tab_manager;

pub use tab_manager::TabManager;

pub use input_manager::InputManager;

pub use overlay_manager::OverlayManager;

pub use split_layout_manager::{
    HANDLE_HIT_PADDING, HANDLE_HIT_SIZE, HANDLE_VISUAL_SIZE, SplitDragState, SplitLayoutManager,
    SplitPaneDrag,
};

pub use polling_coordinator::PollingCoordinator;

pub use render_hooks::{
    queue_emote_popout, refresh_auth_cache, refresh_auth_cache_if_changed, take_pending_popouts,
};
