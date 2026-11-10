//! The app-wide single-line text input (settings fields, search boxes,
//! dialogs and popouts).
//!
//! Provides:
//! - Text editing (cursor, selection, clipboard)
//! - Key bindings (arrows, home/end, backspace, delete)
//! - Event emission (Change, PressEnter, Focus, Blur)
//! - Spell check support
//! - IME support

mod bidi_probe;
mod caret;
mod chrome;
mod element;
mod state;

pub use caret::platform_caret_blink_interval;
pub use chrome::{apply_standard_input_chrome, inline_text_with_cursor};
pub use element::Input;
pub use state::{InputEvent, InputState};
pub(crate) use state::{is_input_focused, normalize_single_line_text};

use gpui::{App, KeyBinding, actions};

actions!(
    echo_input,
    [
        Backspace,
        Delete,
        DeleteWord,
        DeleteWordForward,
        MoveLeft,
        MoveRight,
        MoveWordLeft,
        MoveWordRight,
        MoveToStart,
        MoveToEnd,
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        SelectToStart,
        SelectToEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Escape,
    ]
);

/// Action for Enter key with optional secondary modifier
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
pub struct Enter {
    pub secondary: bool,
}

/// Tab completion action used only by chat composers. These context-specific
/// bindings take precedence over EchoRoot's generic focus traversal so the
/// physical Tab key reaches inline completion first.
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
pub struct TabComplete {
    pub reverse: bool,
}

const CONTEXT: &str = "EchoInput";
pub const CHAT_INPUT_CONTEXT: &str = "ChatInput";
pub const THREAD_REPLY_INPUT_CONTEXT: &str = "ThreadReplyInput";

/// Initialize input key bindings
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("enter", Enter { secondary: false }, Some(CONTEXT)),
        KeyBinding::new("shift-enter", Enter { secondary: true }, Some(CONTEXT)),
        KeyBinding::new("escape", Escape, Some(CONTEXT)),
        KeyBinding::new(
            "tab",
            TabComplete { reverse: false },
            Some(CHAT_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "shift-tab",
            TabComplete { reverse: true },
            Some(CHAT_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "tab",
            TabComplete { reverse: false },
            Some(THREAD_REPLY_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "shift-tab",
            TabComplete { reverse: true },
            Some(THREAD_REPLY_INPUT_CONTEXT),
        ),
        KeyBinding::new("left", MoveLeft, Some(CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(CONTEXT)),
        KeyBinding::new("home", MoveToStart, Some(CONTEXT)),
        KeyBinding::new("end", MoveToEnd, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectToStart, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectToEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-x", Cut, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", Paste, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-z", Undo, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-z", Undo, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-z", Redo, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-y", Redo, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-backspace", DeleteWord, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-backspace", DeleteWord, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-delete", DeleteWordForward, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-delete", DeleteWordForward, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-left", MoveWordLeft, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-left", MoveWordLeft, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-right", MoveWordRight, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-right", MoveWordRight, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-shift-right", SelectWordRight, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-left", MoveToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-right", MoveToEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-left", SelectToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-right", SelectToEnd, Some(CONTEXT)),
    ]);
}
