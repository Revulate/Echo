//! Manager modules extracted from the app.rs monolith
//!
//! This module contains the manager entities that coordinate different
//! aspects of the Echo application:
//!
//! - **TabManager**: Tab bar, drag reorder, overflow, context menu
//! - **InputManager**: Per-split input, history, autocomplete coordination
//! - **OverlayManager**: Modal lifecycle, positioning, z-ordering
//! - **SplitLayoutManager**: Split tree, drag-drop, resize, bounds tracking
//! - **PollingCoordinator**: Background polling loop coordination
//! - **RenderHooks**: Pre-render side effect extraction
//! - **FrameRateController**: Automatic FPS management based on window focus

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
