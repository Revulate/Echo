//! Non-image fallback element widgets for inline boxes, replies, and embeds.

use super::*;

use crate::components::chat::canvas_renderer::layout;

#[inline]
pub(super) fn render_inline_box(
    text: &SharedString,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    bg_color: Hsla,
    text_color: Hsla,
    font_size: f32,
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .rounded(px(2.0))
        .bg(bg_color)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_size(px(font_size))
                .font_weight(FontWeight::BOLD)
                .text_color(text_color)
                .child(text.clone()),
        )
        .into_any_element()
}

/// Render reply indicator with a curved connector
///
/// Creates an elegant curved line from the message to the parent reference,
/// using CSS border-radius for a smooth bezier-like appearance.
#[inline]
pub(super) fn render_reply_indicator(
    connector_x: f32,
    connector_y: f32,
    connector_height: f32,
    parent_display_name: &SharedString,
    parent_color: Hsla,
    preview_text: &SharedString,
    text_x: f32,
    width: f32,
) -> AnyElement {
    use crate::theme::colors as theme_colors;
    use gpui::prelude::FluentBuilder;
    use layout::{REPLY_CONNECTOR_ARM, REPLY_CONNECTOR_WIDTH};

    let curve_radius = 6.0_f32;

    let connector_color = theme_colors::text_muted();
    let sanitized_parent_name = parent_display_name
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\t'))
        .collect::<String>();
    let parent_name_for_render = if sanitized_parent_name.is_empty() {
        parent_display_name.as_ref()
    } else {
        sanitized_parent_name.as_str()
    };
    let parent_mention: SharedString = format!("@{}", parent_name_for_render).into();

    // One flex row so the preview starts after the name's real shaped width;
    // a per-character estimate overlapped long or wide-glyph names.
    let header = div()
        .absolute()
        .left(px(text_x))
        .top(px(1.0))
        .w(px(width.max(0.0)))
        .flex()
        .flex_row()
        .gap(px(8.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_xs()
        .child(
            div()
                .flex_shrink_0()
                .max_w(px(width.max(0.0)))
                .overflow_hidden()
                .text_ellipsis()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(parent_color)
                .child(parent_mention),
        )
        .when(!preview_text.is_empty(), |el| {
            el.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme_colors::text_muted())
                    .child(preview_text.clone()),
            )
        });

    div()
        .absolute()
        .left(px(0.0))
        .top(px(connector_y))
        .w_full()
        .h(px(connector_height + 4.0))
        .child(
            div()
                .absolute()
                .left(px(connector_x))
                .top(px(2.0))
                .w(px(REPLY_CONNECTOR_ARM + curve_radius))
                .h(px(connector_height - 4.0))
                .border_l(px(REPLY_CONNECTOR_WIDTH))
                .border_t(px(REPLY_CONNECTOR_WIDTH))
                .border_color(connector_color)
                .rounded_tl(px(curve_radius)),
        )
        .child(header)
        .into_any_element()
}

/// Render Echo's compact local upload preview. Unlike a rich link card, this
/// contains no fetched metadata and only loads the direct share URL when the
/// upload history recorded an image MIME type.
#[inline]
pub(super) fn render_upload_preview(
    _url: &SharedString,
    thumbnail_url: Option<&SharedString>,
    thumbnail_width: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> AnyElement {
    use crate::theme::colors as theme_colors;

    let thumbnail_height = height.min(56.0);
    let mut thumbnail = div()
        .w(px(thumbnail_width))
        .h(px(thumbnail_height))
        .flex_shrink_0()
        .rounded(px(4.0))
        .overflow_hidden()
        .bg(theme_colors::bg_surface());

    if let Some(thumb_url) = thumbnail_url {
        let cached_thumbnail = crate::cache::loader::get_cached_render_image(thumb_url.as_ref());
        if cached_thumbnail.is_none() {
            crate::cache::loader::trigger_preload_single(
                thumb_url.as_ref(),
                crate::cache::loader::PreloadSource::VisibleChat,
            );
        }
        if let Some(image) = cached_thumbnail {
            thumbnail = thumbnail.child(img(image).size_full().object_fit(ObjectFit::Contain));
        }
    }

    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .rounded(px(4.0))
        .bg(theme_colors::bg_elevated())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .id("upload-preview")
        .role(accesskit::Role::Image)
        .aria_label("Uploaded image preview")
        .overflow_hidden()
        .child(thumbnail)
        .child(
            div()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme_colors::text_primary())
                .child("Uploaded image"),
        )
        .into_any_element()
}

/// Render a Discord-style embed preview card for URLs
#[inline]
pub(super) fn render_embed_preview(
    _url: &SharedString,
    platform: &EmbedPlatform,
    thumbnail_url: Option<&SharedString>,
    title: Option<&SharedString>,
    description: Option<&SharedString>,
    subtitle: Option<&SharedString>,
    duration: Option<&SharedString>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    geometry: &layout::EmbedPreviewGeometry,
    _is_expanded: bool,
    is_loading: bool,
) -> AnyElement {
    use crate::theme::colors as theme_colors;
    use layout::{EMBED_ACCENT_WIDTH, EMBED_BORDER_RADIUS, EMBED_PADDING, EMBED_TITLE_DESC_GAP};

    let accent_color = match platform {
        EmbedPlatform::SevenTv => rgb(0x7A8EFF),
        EmbedPlatform::Spotify => rgb(0x1ED760),
        EmbedPlatform::Steam => rgb(0x1B2838),
        EmbedPlatform::YouTube => rgb(0xFF0000),
        EmbedPlatform::TwitchClip => rgb(0x9146FF),
        EmbedPlatform::TwitchVod | EmbedPlatform::TwitchChannel => rgb(0x9146FF),
        EmbedPlatform::Twitter => rgb(0x1DA1F2),
        EmbedPlatform::Reddit => rgb(0xFF4500),
        EmbedPlatform::Image => rgb(0x5865F2),
        EmbedPlatform::Video => rgb(0x5865F2),
        EmbedPlatform::Generic => {
            if _url.contains("twitch.tv") {
                rgb(0x9146FF)
            } else {
                theme_colors::text_secondary().into()
            }
        }
    };

    let mut container = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .rounded(px(EMBED_BORDER_RADIUS))
        .bg(theme_colors::bg_elevated())
        .border_l(px(EMBED_ACCENT_WIDTH))
        .border_color(accent_color)
        .cursor_pointer()
        .overflow_hidden();

    if is_loading {
        container = container.child(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .py(px(EMBED_PADDING))
                .child(
                    div()
                        .text_color(theme_colors::text_secondary())
                        .text_sm()
                        .child("Loading…"),
                ),
        );
    } else {
        let has_thumbnail = geometry.thumbnail_width > 0.0;

        let mut content = div().size_full().flex().p(px(EMBED_PADDING));

        if has_thumbnail && !geometry.thumbnail_stacked {
            content = content.flex_row().gap(px(geometry.thumbnail_gap));
        } else {
            content = content.flex_col();
            if has_thumbnail {
                content = content.gap(px(geometry.thumbnail_gap));
            }
        }

        if let Some(thumb_url) = thumbnail_url.filter(|_| has_thumbnail) {
            let is_video = matches!(
                platform,
                EmbedPlatform::YouTube
                    | EmbedPlatform::TwitchClip
                    | EmbedPlatform::TwitchVod
                    | EmbedPlatform::Video
            );

            let mut thumb_container = div()
                .rounded(px(4.0))
                .overflow_hidden()
                .flex_shrink_0()
                .relative();

            thumb_container = thumb_container
                .w(px(geometry.thumbnail_width))
                .h(px(geometry.thumbnail_height));

            let cached_thumbnail =
                crate::cache::loader::get_cached_render_image(thumb_url.as_ref());
            if cached_thumbnail.is_none() {
                crate::cache::loader::trigger_preload_single(
                    thumb_url.as_ref(),
                    crate::cache::loader::PreloadSource::VisibleChat,
                );
            }
            match cached_thumbnail {
                Some(image) => {
                    let img_element = img(image).size_full().object_fit(if is_video {
                        ObjectFit::Cover
                    } else {
                        ObjectFit::Contain
                    });
                    thumb_container = thumb_container.child(img_element);
                }
                None => {
                    thumb_container =
                        thumb_container.child(div().size_full().bg(theme_colors::bg_surface()));
                }
            }

            if let Some(d) = duration {
                thumb_container = thumb_container.child(
                    div()
                        .absolute()
                        .bottom(px(4.0))
                        .right(px(4.0))
                        .px(px(4.0))
                        .py(px(2.0))
                        .bg(hsla(0.0, 0.0, 0.0, 0.8))
                        .rounded(px(3.0))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(white())
                        .child(d.to_string()),
                );
            }

            content = content.child(thumb_container);
        }

        let mut text_section = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(EMBED_TITLE_DESC_GAP))
            .overflow_hidden();

        if has_thumbnail && !geometry.thumbnail_stacked {
            text_section = text_section.justify_center();
        }

        let platform_name = match platform {
            EmbedPlatform::SevenTv => "7TV",
            EmbedPlatform::Spotify => "SPOTIFY",
            EmbedPlatform::Steam => "STEAM",
            EmbedPlatform::YouTube => "YOUTUBE",
            EmbedPlatform::TwitchClip => "TWITCH CLIP",
            EmbedPlatform::TwitchVod => "TWITCH VOD",
            EmbedPlatform::TwitchChannel => "TWITCH CHANNEL",
            EmbedPlatform::Twitter => "TWITTER",
            EmbedPlatform::Reddit => "REDDIT",
            EmbedPlatform::Image => "IMAGE",
            EmbedPlatform::Video => "VIDEO",
            EmbedPlatform::Generic => "LINK",
        };

        text_section = text_section.child(
            div()
                .text_xs()
                .line_height(px(14.0))
                .text_color(theme_colors::text_muted())
                .font_weight(FontWeight::SEMIBOLD)
                .child(platform_name),
        );

        if let Some(t) = title.filter(|_| geometry.title_lines > 0) {
            text_section = text_section.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme_colors::text_primary())
                    .line_height(px(18.0))
                    .line_clamp(geometry.title_lines as usize)
                    .child(t.to_string()),
            );
        }

        if let Some(d) = description.filter(|_| geometry.description_lines > 0) {
            text_section = text_section.child(
                div()
                    .text_xs()
                    .text_color(theme_colors::text_secondary())
                    .line_height(px(16.0))
                    .line_clamp(geometry.description_lines as usize)
                    .child(d.to_string()),
            );
        }

        if let Some(s) = subtitle.filter(|_| geometry.subtitle_lines > 0) {
            text_section = text_section.child(
                div()
                    .text_xs()
                    .text_color(theme_colors::text_secondary())
                    .line_height(px(16.0))
                    .line_clamp(geometry.subtitle_lines as usize)
                    .child(s.to_string()),
            );
        }

        content = content.child(text_section);
        container = container.child(content);
    }

    container.into_any_element()
}
