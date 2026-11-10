//! Root view of the main window.
//!
//! Wraps the application view in the `EchoRoot` key context, which binds
//! Tab/Shift-Tab to focus traversal between tab stops, and samples renderer
//! statistics on every main-window draw.

use gpui::{
    AnyView, App, Context, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _,
    Render, Styled, Window, actions, div,
};

actions!(echo_root, [Tab, TabPrev]);

const CONTEXT: &str = "EchoRoot";

/// Initialize Root key bindings (call once at startup)
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", Tab, Some(CONTEXT)),
        KeyBinding::new("shift-tab", TabPrev, Some(CONTEXT)),
    ]);
}

/// Root is the main window's top-level wrapper view.
///
/// It wraps the main application view and provides:
/// - Tab navigation between focusable elements
/// - A consistent container structure
/// - Per-draw renderer statistics sampling (it renders on every main-window draw)
pub struct Root {
    view: AnyView,
}

impl Root {
    /// Create a new Root wrapping the given view.
    pub fn new(view: impl Into<AnyView>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { view: view.into() }
    }

    /// Whether keyboard focus is already on a tab stop, i.e. the user is
    /// moving through controls with the keyboard. Tab from anywhere else (a
    /// chat canvas, a pane, an overlay's text field, nothing) must not pull
    /// focus onto a button, where typed text would no longer reach chat.
    fn navigating_tab_stops(window: &Window, cx: &App) -> bool {
        window.focused(cx).is_some_and(|focused| focused.tab_stop)
    }

    fn on_action_tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        if Self::navigating_tab_stops(window, cx) {
            window.focus_next(cx);
        } else {
            // Let the focused surface's own key handling see Tab.
            cx.propagate();
        }
    }

    fn on_action_tab_prev(&mut self, _: &TabPrev, window: &mut Window, cx: &mut Context<Self>) {
        if Self::navigating_tab_stops(window, cx) {
            window.focus_prev(cx);
        } else {
            cx.propagate();
        }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::diagnostics::fps_profiler::record_main_window_renderer_statistics(window);
        div()
            .id("root")
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_action_tab))
            .on_action(cx.listener(Self::on_action_tab_prev))
            .relative()
            .size_full()
            .child(self.view.clone())
    }
}
