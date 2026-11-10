//! Reusable profile avatar component
//!
//! Renders a user's profile picture with fallback to initials.
//!
//! Note: Size variants for planned account switcher popout.

use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::*;

/// Render a profile avatar with image or initials fallback
///
/// # Arguments
/// * `profile_image_url` - Optional URL to the user's profile picture
/// * `display_name` - The user's display name (used for initials fallback)
/// * `size_px` - Size of the avatar in pixels
pub fn render_profile_avatar(
    profile_image_url: Option<&str>,
    display_name: &str,
    size_px: f32,
) -> impl IntoElement + use<> {
    let size = px(size_px);
    let font_size = px(size_px * 0.45);
    let profile_image_url = if crate::backend::streamer_mode::should_hide_avatars() {
        None
    } else {
        profile_image_url
    };

    if let Some(url) = profile_image_url {
        div()
            .w(size)
            .h(size)
            .rounded(Radii::full())
            .overflow_hidden()
            .flex_shrink_0()
            .bg(colors::bg_elevated())
            .child(
                img(SharedString::from(url.to_string()))
                    .w(size)
                    .h(size)
                    .object_fit(ObjectFit::Cover),
            )
    } else {
        let initial = display_name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();

        div()
            .w(size)
            .h(size)
            .rounded(Radii::full())
            .flex_shrink_0()
            .bg(colors::accent())
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_color(colors::text_on_accent_contrast())
                    .font_weight(FontWeight::BOLD)
                    .text_size(font_size)
                    .child(initial),
            )
    }
}
