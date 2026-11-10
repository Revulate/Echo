//! Chat components
//!
//! Components for rendering chat messages, input, and related UI.

pub mod canvas_renderer;
pub mod channel_popup;
pub mod chat_header_renderer;
pub mod chat_input_renderer;
pub mod chat_pane;
pub mod chat_settings_bar;
pub mod chat_ui;
pub mod chat_view;
pub mod scroll_map;

pub use channel_popup::{open_channel_popup, restore_open_channel_popups};
pub use chat_header_renderer::ChatHeaderRenderer;
pub use chat_input_renderer::ChatInputRenderer;
pub use chat_pane::render_chat_pane;
pub use chat_view::{ChatView, ChatViewDirtyReason};
