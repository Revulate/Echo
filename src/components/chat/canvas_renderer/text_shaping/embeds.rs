//! Embed card geometry and platform styling.

use super::*;

/// A row layout must leave enough room for a short, useful run of card text.
/// Otherwise the thumbnail moves above the text instead of squeezing it into a
/// column of one-word lines.
pub(super) const EMBED_MIN_HORIZONTAL_TEXT_WIDTH: f32 = 120.0;

pub(super) const EMBED_TEXT_ONLY_MIN_HEIGHT: f32 = 76.0;

pub(super) const EMBED_PLATFORM_LINE_HEIGHT: f32 = 14.0;

pub(super) const EMBED_TITLE_LINE_HEIGHT: f32 = 18.0;

pub(super) const EMBED_BODY_LINE_HEIGHT: f32 = 16.0;

pub(super) const EMBED_TITLE_MAX_LINES: u8 = 2;

pub(super) const EMBED_BODY_MAX_LINES: u8 = 3;

/// Count the clamped lines an embed text field needs at its shaped width. The
/// painter uses the same count and fixed line height, keeping the element tree
/// within the exact height reserved by the message layout.
/// The card style for a link that has no richer media kind.
pub(super) fn embed_platform_for(
    link_type: &crate::backend::link_types::LinkType,
) -> crate::components::chat::canvas_renderer::layout::EmbedPlatform {
    use crate::backend::link_types::LinkType;
    use crate::components::chat::canvas_renderer::layout::EmbedPlatform;
    match link_type {
        LinkType::SevenTvEmote | LinkType::SevenTvUser => EmbedPlatform::SevenTv,
        LinkType::Spotify => EmbedPlatform::Spotify,
        LinkType::Steam => EmbedPlatform::Steam,
        LinkType::YouTube => EmbedPlatform::YouTube,
        LinkType::TwitchClip => EmbedPlatform::TwitchClip,
        LinkType::TwitchVod => EmbedPlatform::TwitchVod,
        LinkType::TwitchChannel => EmbedPlatform::TwitchChannel,
        LinkType::Twitter => EmbedPlatform::Twitter,
        LinkType::Reddit => EmbedPlatform::Reddit,
        LinkType::Image => EmbedPlatform::Image,
        LinkType::Video => EmbedPlatform::Video,
        LinkType::Generic => EmbedPlatform::Generic,
    }
}

pub(super) fn embed_text_line_count(
    builder: &mut LayoutBuilder<'_>,
    text: Option<&str>,
    text_width: f32,
    is_bold: bool,
    max_lines: u8,
) -> u8 {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return 0;
    };
    let measured_width = builder.measure_text_width(text, is_bold);
    (measured_width / text_width.max(1.0))
        .ceil()
        .clamp(1.0, f32::from(max_lines)) as u8
}

pub(super) fn preview_source_dimensions(
    builder: &mut LayoutBuilder<'_>,
    url: &str,
    fallback: (f32, f32),
) -> (f32, f32) {
    if let Some((width, height)) =
        crate::components::chat::canvas_renderer::native_dimensions::decoded_image_size(url)
    {
        (width as f32, height as f32)
    } else {
        builder.track_pending_native_dimension_url(url);
        fallback
    }
}

/// The thumbnail mode caps height; native aspect ratio determines width.
pub(super) fn shape_embed_preview_geometry(
    builder: &mut LayoutBuilder<'_>,
    preview_width: f32,
    metadata: &crate::backend::link_types::EmbedMetadata,
) -> (EmbedPreviewGeometry, f32) {
    let inner_width = (preview_width - EMBED_ACCENT_WIDTH - EMBED_PADDING * 2.0).max(0.0);
    let thumbnail_mode = settings::get_embed_thumbnail_mode();
    let thumbnail_visible = !crate::backend::streamer_mode::should_hide_link_thumbnails()
        && crate::backend::embed_thumbnails::should_render_thumbnail(
            thumbnail_mode,
            metadata.thumbnail_url.as_deref(),
        )
        && inner_width > 0.0;
    let (thumbnail_width, thumbnail_height) = thumbnail_visible
        .then(|| crate::backend::embed_thumbnails::thumbnail_dimensions(thumbnail_mode))
        .flatten()
        .map(|(configured_width, configured_height)| {
            let (source_width, source_height) = preview_source_dimensions(
                builder,
                metadata.thumbnail_url.as_deref().unwrap_or_default(),
                (configured_width, configured_height),
            );
            crate::backend::embed_thumbnails::fit_thumbnail_to_height(
                source_width,
                source_height,
                inner_width,
                configured_height,
            )
        })
        .unwrap_or((0.0, 0.0));
    let thumbnail_stacked = thumbnail_width > 0.0
        && inner_width - thumbnail_width - EMBED_TEXT_THUMBNAIL_GAP
            < EMBED_MIN_HORIZONTAL_TEXT_WIDTH;
    let text_width = if thumbnail_width > 0.0 && !thumbnail_stacked {
        inner_width - thumbnail_width - EMBED_TEXT_THUMBNAIL_GAP
    } else {
        inner_width
    };
    let title_lines = embed_text_line_count(
        builder,
        metadata.title.as_deref(),
        text_width,
        true,
        EMBED_TITLE_MAX_LINES,
    );
    let description_lines = embed_text_line_count(
        builder,
        metadata.description.as_deref(),
        text_width,
        false,
        EMBED_BODY_MAX_LINES,
    );
    let subtitle_lines = embed_text_line_count(
        builder,
        metadata.subtitle.as_deref(),
        text_width,
        false,
        EMBED_BODY_MAX_LINES,
    );

    let mut text_height = EMBED_PLATFORM_LINE_HEIGHT;
    let mut text_field_count = 1;
    for (lines, line_height) in [
        (title_lines, EMBED_TITLE_LINE_HEIGHT),
        (description_lines, EMBED_BODY_LINE_HEIGHT),
        (subtitle_lines, EMBED_BODY_LINE_HEIGHT),
    ] {
        if lines > 0 {
            text_height += f32::from(lines) * line_height;
            text_field_count += 1;
        }
    }
    text_height += (text_field_count - 1) as f32 * EMBED_TITLE_DESC_GAP;

    let content_height = if thumbnail_width > 0.0 {
        if thumbnail_stacked {
            thumbnail_height + EMBED_TEXT_THUMBNAIL_GAP + text_height
        } else {
            thumbnail_height.max(text_height)
        }
    } else {
        text_height
    };
    let minimum_height = if thumbnail_width > 0.0 {
        EMBED_MIN_HEIGHT
    } else {
        EMBED_TEXT_ONLY_MIN_HEIGHT
    };
    let height = (content_height + EMBED_PADDING * 2.0).max(minimum_height);

    (
        EmbedPreviewGeometry {
            thumbnail_width,
            thumbnail_height,
            thumbnail_stacked,
            thumbnail_gap: EMBED_TEXT_THUMBNAIL_GAP,
            title_lines,
            description_lines,
            subtitle_lines,
        },
        height,
    )
}
