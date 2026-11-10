//! Shared root for every Echo-owned native GPUI window.
//!
//! `img()` resolves generic URI assets through a tree-scoped image-cache
//! provider. Keeping that provider here makes the cache policy a property of
//! creating an Echo native window, rather than a convention each popout must
//! remember to reproduce in its own render tree. The root also reports the
//! window's activation, so Echo knows whether any of its windows is in front.

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Result, Styled as _, Subscription, Window, WindowHandle, WindowId,
    WindowOptions, div,
};

/// Opens an Echo-owned native window with the shared URI image cache installed
/// at its outermost view boundary.
///
/// All application `open_window` calls must use this helper. The returned root
/// entity deliberately contains the feature-specific view, so callers keep
/// their existing native-window lifecycle behavior while URI-backed GPUI
/// images share Echo's bounded cache policy.
pub fn open_native_window<V: 'static + Render>(
    cx: &mut App,
    options: WindowOptions,
    build_root_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<WindowHandle<NativeWindowRoot>> {
    cx.open_window(options, |window, cx| {
        let view = build_root_view(window, cx);
        cx.new(|cx| NativeWindowRoot::new(view, window, cx))
    })
}

/// A cache-owning native-window root that renders an arbitrary Echo view.
pub struct NativeWindowRoot {
    view: AnyView,
    window_id: WindowId,
    _activation: Subscription,
}

impl NativeWindowRoot {
    fn new(view: impl Into<AnyView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_id = window.window_handle().window_id();
        crate::utils::record_window_activation(window_id, window.is_window_active());
        let activation = cx.observe_window_activation(window, move |_, window, _| {
            crate::utils::record_window_activation(window_id, window.is_window_active());
        });
        Self {
            view: view.into(),
            window_id,
            _activation: activation,
        }
    }
}

impl Drop for NativeWindowRoot {
    fn drop(&mut self) {
        // A window closed while active never reports its deactivation.
        crate::utils::record_window_activation(self.window_id, false);
    }
}

impl Render for NativeWindowRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .image_cache(crate::cache::uri_layer::provider(cx))
            .id("echo-native-window-root")
            .relative()
            .size_full();
        // Popouts take their text family from here. On Linux GPUI's
        // `.SystemUIFont` names IBM Plex Sans, which Echo does not bundle.
        #[cfg(target_os = "linux")]
        let root = root.font_family(echo_chat_render::text_run_intern::ui_font_family());
        root.child(self.view.clone())
    }
}
