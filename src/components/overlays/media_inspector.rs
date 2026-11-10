//! Reusable native inspector for image media attached to rich embed cards.
//!
//! The inspector intentionally uses only GPUI's URI image element beneath
//! Echo's shared native-window root. That root supplies the process-wide,
//! bounded URI image cache; this module owns no downloader, cache, player, or
//! persisted window preference.

use crate::components::ui::{Retarget, SingletonWindow};
use crate::theme::colors;
use crate::theme::tokens::Spacing;
use gpui::*;

const MEDIA_INSPECTOR_SIZE: (f32, f32) = (960.0, 720.0);
const MEDIA_INSPECTOR_MIN_SIZE: (f32, f32) = (320.0, 240.0);

/// Pending payload: the image source to show next.
static MEDIA_INSPECTOR: SingletonWindow<String> = SingletonWindow::new();

/// Open the reusable inspector, or retarget and focus the existing window.
///
/// The caller only supplies a `MediaKind::Image` source that was already
/// classified while shaping the rich card. This does not open the card's
/// external browser target and deliberately has no playback path.
pub fn open_media_inspector(source_url: String, cx: &mut App) {
    if source_url.trim().is_empty() || crate::backend::streamer_mode::should_hide_link_thumbnails()
    {
        return;
    }

    let source_url = match MEDIA_INSPECTOR.retarget(source_url, cx) {
        Retarget::Retargeted => return,
        Retarget::NotOpen(source_url) => source_url,
        Retarget::Stale(source_url) => {
            MEDIA_INSPECTOR.mark_closed();
            source_url
        }
    };

    let window_size = size(px(MEDIA_INSPECTOR_SIZE.0), px(MEDIA_INSPECTOR_SIZE.1));
    let (bounds, display_id) = crate::utils::popup_window_placement(None, window_size, cx);
    let open_result = crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Image preview".into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Floating,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(
                px(MEDIA_INSPECTOR_MIN_SIZE.0),
                px(MEDIA_INSPECTOR_MIN_SIZE.1),
            )),
            ..Default::default()
        },
        move |window, cx| {
            MEDIA_INSPECTOR.set_handle(window.window_handle());
            MEDIA_INSPECTOR.mark_open();
            cx.new(|cx| {
                let inspector = MediaInspector::new(source_url.clone(), cx);
                window.focus(&inspector.focus_handle, cx);
                inspector
            })
        },
    );

    if open_result.is_err() {
        MEDIA_INSPECTOR.mark_closed();
    }
}

pub struct MediaInspector {
    source_url: SharedString,
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
    focus_handle: FocusHandle,
}

impl MediaInspector {
    fn new(source_url: String, cx: &mut Context<Self>) -> Self {
        Self {
            source_url: SharedString::from(source_url),
            always_on_top: Default::default(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn apply_pending_source(&mut self) {
        let next_source = MEDIA_INSPECTOR.take_pending();
        if let Some(source_url) = next_source.filter(|source_url| !source_url.trim().is_empty()) {
            self.source_url = SharedString::from(source_url);
        }
    }
}

impl Drop for MediaInspector {
    fn drop(&mut self) {
        MEDIA_INSPECTOR.mark_closed();
    }
}

impl Render for MediaInspector {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.always_on_top.sync(window);
        self.apply_pending_source();

        let hidden_by_streamer_mode = crate::backend::streamer_mode::should_hide_link_thumbnails();
        div()
            .id("media-inspector")
            .track_focus(&self.focus_handle)
            .on_key_down(|event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    window.remove_window();
                }
            })
            .size_full()
            .p(px(Spacing::MD))
            .bg(colors::bg_void())
            .flex()
            .items_center()
            .justify_center()
            .child(if hidden_by_streamer_mode {
                div()
                    .text_color(colors::text_secondary())
                    .text_sm()
                    .child("Image preview hidden by streamer mode")
                    .into_any_element()
            } else {
                img(self.source_url.clone())
                    .id("media-inspector-image")
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            })
    }
}
