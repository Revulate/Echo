//! Clipboard utilities for Echo GPUI
//! Used by ContextMenu for copy operations
//!
//! Linux goes through GPUI's own clipboard, which speaks native Wayland and
//! X11. arboard speaks only X11: under Plasma's Wayland session KWin lets an
//! X11 client read the clipboard only while an X11 window is focused, so a
//! paste through it saw nothing or Echo's own stale copy, and its copies
//! vanished once the handle serving them dropped. Copies made without an `App`
//! at hand reach GPUI through a UI-thread writer.

#[cfg(not(target_os = "linux"))]
use arboard::Clipboard;
use gpui::App;

#[cfg(target_os = "linux")]
static UI_CLIPBOARD_WRITER: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<String>> =
    std::sync::OnceLock::new();

/// Start the UI-thread clipboard writer that Linux copies go through. Call
/// once, when GPUI starts.
pub fn install_platform_clipboard(cx: &mut App) {
    #[cfg(target_os = "linux")]
    {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        if UI_CLIPBOARD_WRITER.set(sender).is_err() {
            return;
        }
        cx.spawn(async move |cx| {
            while let Some(text) = receiver.recv().await {
                cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)));
            }
        })
        .detach();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}

/// Replace the system clipboard with `text`.
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        UI_CLIPBOARD_WRITER
            .get()
            .ok_or_else(|| "Failed to write clipboard: Echo's clipboard is not ready".to_string())?
            .send(text.to_owned())
            .map_err(|_| "Failed to write clipboard: Echo's clipboard writer stopped".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text))
            .map_err(|error| format!("Failed to write clipboard: {error}"))
    }
}

/// Copy text to the system clipboard
pub fn copy_text(text: &str) {
    match set_clipboard_text(text) {
        Ok(()) => crate::log_ui!(info, "Copied {} bytes to clipboard", text.len()),
        Err(error) => crate::log_ui!(error, "{}", error),
    }
}

/// Read plain text for paste actions.
pub fn read_text(cx: &App) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        cx.read_from_clipboard().and_then(|item| item.text())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cx;
        match Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => Some(text),
            Err(arboard::Error::ContentNotAvailable) => None,
            Err(error) => {
                crate::log_ui!(error, "Failed to read text from clipboard: {}", error);
                None
            }
        }
    }
}
