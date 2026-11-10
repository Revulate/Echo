use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EmbedThumbnailMode {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "small")]
    Small,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "large")]
    Large,
}

impl EmbedThumbnailMode {
    pub fn settings_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    pub fn from_settings_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "small" | "compact" | "96" | "96x54" => Self::Small,
            "medium" | "normal" | "160" | "160x90" => Self::Medium,
            "large" | "big" | "240" | "240x135" => Self::Large,
            _ => Self::None,
        }
    }

    pub fn shows_thumbnails(self) -> bool {
        !matches!(self, Self::None)
    }
}

pub fn thumbnail_dimensions(mode: EmbedThumbnailMode) -> Option<(f32, f32)> {
    match mode {
        EmbedThumbnailMode::None => None,
        EmbedThumbnailMode::Small => Some((96.0, 54.0)),
        EmbedThumbnailMode::Medium => Some((160.0, 90.0)),
        EmbedThumbnailMode::Large => Some((240.0, 135.0)),
    }
}

/// Preserve aspect ratio while using the available width up to the height cap.
pub fn fit_thumbnail_to_height(
    source_width: f32,
    source_height: f32,
    max_width: f32,
    max_height: f32,
) -> (f32, f32) {
    if [source_width, source_height, max_width, max_height]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return (0.0, 0.0);
    }
    let scale = (max_width / source_width).min(max_height / source_height);
    (
        (source_width * scale).min(max_width),
        (source_height * scale).min(max_height),
    )
}

pub fn is_valid_embed_thumbnail_url(url: &str) -> bool {
    let lower_url = url.to_ascii_lowercase();
    !(lower_url.contains("v.redd.it") || lower_url.contains("external-preview.redd.it"))
}

pub fn should_render_thumbnail(mode: EmbedThumbnailMode, thumbnail_url: Option<&str>) -> bool {
    mode.shows_thumbnails()
        && thumbnail_url
            .map(is_valid_embed_thumbnail_url)
            .unwrap_or(false)
}

pub fn preview_height(
    mode: EmbedThumbnailMode,
    has_renderable_thumbnail: bool,
    padding: f32,
    min_thumbnail_height: f32,
    text_only_height: f32,
) -> f32 {
    if has_renderable_thumbnail {
        let thumbnail_height = thumbnail_dimensions(mode)
            .map(|(_, height)| height)
            .unwrap_or(0.0);
        (thumbnail_height + padding * 2.0).max(min_thumbnail_height)
    } else {
        text_only_height
    }
}
