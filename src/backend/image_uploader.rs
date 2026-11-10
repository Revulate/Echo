//! Application-side image upload transport.
//!
//! The uploader configuration, response-template
//! resolution, and upload-policy helpers are transport-independent and live in
//! `echo-link-core`; this module owns PNG encoding and the multipart request.

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::multipart::{Form, Part};
use std::time::Duration;

pub use echo_link_core::image_uploader::*;

const UPLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// How local image files reached the uploader; each way has its own toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFileSource {
    Dropped,
    Pasted,
}

impl ImageFileSource {
    pub fn upload_enabled(self, config: &ImageUploaderConfig) -> bool {
        config.enabled
            && match self {
                Self::Dropped => config.drag_drop_upload_enabled,
                Self::Pasted => config.paste_upload_enabled,
            }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::Pasted => "pasted",
        }
    }
}

/// Whether a pasted image of `source_bytes` needs confirmation: always while
/// "ask before every paste upload" is on, otherwise only for large images.
pub fn should_confirm_paste_upload(config: &ImageUploaderConfig, source_bytes: u64) -> bool {
    super::settings::get_ask_before_paste_upload()
        || should_confirm_upload_size(config, source_bytes)
}
const MAX_UPLOAD_RESPONSE_BYTES: usize = 256 * 1024;

pub fn encode_rgba_png(width: usize, height: usize, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Cannot upload an empty image".to_string());
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Image dimensions are too large".to_string())?;
    if rgba.len() != expected_len {
        return Err(format!(
            "Clipboard image buffer has {} bytes, expected {} for {}x{} RGBA",
            rgba.len(),
            expected_len,
            width,
            height
        ));
    }

    let width = u32::try_from(width).map_err(|_| "Image width is too large".to_string())?;
    let height = u32::try_from(height).map_err(|_| "Image height is too large".to_string())?;
    let mut output = Vec::with_capacity((rgba.len() / 2).max(4096));
    let encoder = PngEncoder::new(&mut output);
    encoder
        .write_image(&rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| format!("Failed to encode clipboard image as PNG: {e}"))?;
    Ok(output)
}

pub async fn upload_image_bytes(
    config: &ImageUploaderConfig,
    filename: &str,
    mime_type: &str,
    bytes: Vec<u8>,
) -> Result<UploadResult, String> {
    prepare_upload_config(config)?;
    if bytes.is_empty() {
        return Err("Cannot upload an empty image".to_string());
    }
    if bytes.len() as u64 > config.max_upload_bytes {
        return Err(format!(
            "Image is larger than the configured {} byte limit",
            config.max_upload_bytes
        ));
    }

    let headers = build_header_map(&config.extra_headers)?;
    let part = Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(mime_type)
        .map_err(|e| format!("Invalid upload MIME type: {e}"))?;
    let form = Form::new().part(config.form_field.trim().to_string(), part);

    tokio::time::timeout(UPLOAD_TOTAL_TIMEOUT, async move {
        let response = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Net,
            crate::backend::http::client()
                .post(config.request_url.trim())
                .headers(headers)
                .multipart(form),
        )
        .await
        .map_err(|e| format!("Upload request failed: {}", e.without_url()))?;

        let status = response.status();
        let response_text = read_bounded_response_text(response).await?;
        if !status.is_success() {
            let detail = upload_error_detail_for_config(&response_text, config);
            return if detail.is_empty() {
                Err(format!("Upload failed with status {status}"))
            } else {
                Err(format!("Upload failed with status {status}: {detail}"))
            };
        }

        parse_upload_response(
            &response_text,
            &config.image_link_template,
            &config.deletion_link_template,
        )
    })
    .await
    .map_err(|_| {
        format!(
            "Upload timed out after {} seconds",
            UPLOAD_TOTAL_TIMEOUT.as_secs()
        )
    })?
}

fn prepare_upload_config(config: &ImageUploaderConfig) -> Result<(), String> {
    if !config.enabled {
        return Err("Image uploader is disabled".to_string());
    }
    if has_placeholder_uploader_key(config) {
        return Err("Replace the uploader API-key placeholder before uploading".to_string());
    }
    validate_config(config)
}

async fn read_bounded_response_text(mut response: reqwest::Response) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_UPLOAD_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "Upload response exceeded the {} KiB limit",
            MAX_UPLOAD_RESPONSE_BYTES / 1024
        ));
    }

    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(MAX_UPLOAD_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(initial_capacity);
    loop {
        let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("Failed to read upload response: {}", e.without_url()))?
        else {
            break;
        };
        if chunk.len() > MAX_UPLOAD_RESPONSE_BYTES - body.len() {
            return Err(format!(
                "Upload response exceeded the {} KiB limit",
                MAX_UPLOAD_RESPONSE_BYTES / 1024
            ));
        }
        body.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn build_header_map(input: &str) -> Result<HeaderMap, String> {
    let mut map = HeaderMap::new();
    for (name, value) in parse_extra_headers(input)? {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("Invalid header name '{name}': {e}"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|e| format!("Invalid value for header '{name}': {e}"))?;
        map.insert(name, value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn placeholder_mapping_is_refused_before_transport() {
        let config = ImageUploaderConfig {
            enabled: true,
            extra_headers: format!("X-API-Key: {UPLOADER_API_KEY_PLACEHOLDER}"),
            ..ImageUploaderConfig::default()
        };
        assert_eq!(
            prepare_upload_config(&config).unwrap_err(),
            "Replace the uploader API-key placeholder before uploading"
        );
    }

    #[test]
    fn disabled_mapping_is_refused_without_validating_its_fields() {
        let config = ImageUploaderConfig {
            enabled: false,
            request_url: "upload.example.com".to_string(),
            extra_headers: "Authorization Bearer abc".to_string(),
            ..ImageUploaderConfig::default()
        };
        assert_eq!(
            prepare_upload_config(&config).unwrap_err(),
            "Image uploader is disabled"
        );
    }
}

/// Resolve the public media thumbnail for upload-result rows.
///
/// Most uploaders return a direct media URL, which is the loader source as-is.
/// Olrite's `{url}` response field is a public HTML share page; its page
/// metadata exposes the stable `/thumb/<id>/400.webp` media endpoint.
/// Using that media endpoint avoids trying to decode the HTML response as an
/// image while keeping deletion URLs out of this path.
pub fn upload_thumbnail_url(image_url: &str) -> String {
    let Ok(parsed) = url::Url::parse(image_url) else {
        return image_url.to_string();
    };
    let is_olrite_share = parsed
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("olrite.lol"))
        && parsed.path_segments().is_some_and(|mut segments| {
            matches!((segments.next(), segments.next()), (Some(id), None) if !id.is_empty())
        });
    if is_olrite_share {
        return format!(
            "{}://olrite.lol/thumb{}/400.webp",
            parsed.scheme(),
            parsed.path()
        );
    }
    image_url.to_string()
}
