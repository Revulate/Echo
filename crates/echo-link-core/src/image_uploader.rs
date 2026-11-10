//! Image uploader configuration (ShareX-style presets), response templates,
//! and upload-policy helpers.
//!
//! Everything here is transport-independent: preset import/export, response
//! link resolution, header-list parsing, size thresholds, and error-detail
//! sanitizing. The multipart request itself stays with the application.

use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

const DEFAULT_FORM_FIELD: &str = "file";
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
pub const KAPPA_UPLOAD_URL: &str = "https://segs.lol/api/upload";
pub const KAPPA_IMAGE_LINK_TEMPLATE: &str = "{link}";
pub const KAPPA_DELETION_LINK_TEMPLATE: &str = "{delete}";
pub const UPLOADER_API_KEY_PLACEHOLDER: &str = "YOUR_API_KEY_HERE";
const MIN_LARGE_UPLOAD_CONFIRM_BYTES: u64 = 1024 * 1024;
const MAX_UPLOAD_ERROR_DETAIL_BYTES: usize = 3 * 1024;

fn default_form_field() -> String {
    DEFAULT_FORM_FIELD.to_string()
}

fn default_true() -> bool {
    true
}

fn default_max_upload_bytes() -> u64 {
    DEFAULT_MAX_UPLOAD_BYTES
}

/// Configurable multipart uploader: request URL, form field, extra headers,
/// and image/deletion link templates.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageUploaderConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub paste_upload_enabled: bool,
    #[serde(default)]
    pub drag_drop_upload_enabled: bool,
    #[serde(default = "default_true")]
    pub copy_link_after_upload: bool,
    #[serde(default = "default_true")]
    pub ask_before_large_upload: bool,
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    #[serde(default)]
    pub request_url: String,
    #[serde(default = "default_form_field")]
    pub form_field: String,
    #[serde(default)]
    pub extra_headers: String,
    #[serde(default)]
    pub image_link_template: String,
    #[serde(default)]
    pub deletion_link_template: String,
}

/// `extra_headers` values are API keys, so `Debug` shows only header names.
impl std::fmt::Debug for ImageUploaderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageUploaderConfig")
            .field("enabled", &self.enabled)
            .field("paste_upload_enabled", &self.paste_upload_enabled)
            .field("drag_drop_upload_enabled", &self.drag_drop_upload_enabled)
            .field("copy_link_after_upload", &self.copy_link_after_upload)
            .field("ask_before_large_upload", &self.ask_before_large_upload)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("request_url", &redact_url_query_values(&self.request_url))
            .field("form_field", &self.form_field)
            .field(
                "extra_headers",
                &redact_extra_header_values(&self.extra_headers),
            )
            .field("image_link_template", &self.image_link_template)
            .field("deletion_link_template", &self.deletion_link_template)
            .finish()
    }
}

impl Default for ImageUploaderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paste_upload_enabled: false,
            drag_drop_upload_enabled: false,
            copy_link_after_upload: true,
            ask_before_large_upload: true,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            request_url: KAPPA_UPLOAD_URL.to_string(),
            form_field: default_form_field(),
            extra_headers: String::new(),
            image_link_template: KAPPA_IMAGE_LINK_TEMPLATE.to_string(),
            deletion_link_template: KAPPA_DELETION_LINK_TEMPLATE.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub image_link: String,
    pub deletion_link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUploadResponse {
    pub image_link: String,
    pub deletion_link: Option<String>,
}

/// Export the uploader as a ShareX-compatible JSON preset.
///
/// The top-level fields follow the ShareX custom-uploader layout, so other
/// tools can read them, while the optional `Echo` object preserves Echo's
/// extra safety and interaction controls when a preset is round-tripped back
/// into Echo.
pub fn export_uploader_config_json(config: &ImageUploaderConfig) -> Result<String, String> {
    let mut root = Map::new();
    root.insert("Version".to_string(), Value::String("1.0.0".to_string()));
    root.insert(
        "Name".to_string(),
        Value::String("Echo Image Uploader Settings".to_string()),
    );
    root.insert(
        "RequestMethod".to_string(),
        Value::String("POST".to_string()),
    );
    root.insert(
        "RequestURL".to_string(),
        Value::String(config.request_url.clone()),
    );
    root.insert(
        "Body".to_string(),
        Value::String("MultipartFormData".to_string()),
    );
    root.insert(
        "FileFormName".to_string(),
        Value::String(config.form_field.clone()),
    );
    root.insert(
        "URL".to_string(),
        Value::String(config.image_link_template.clone()),
    );
    root.insert(
        "DeletionURL".to_string(),
        Value::String(config.deletion_link_template.clone()),
    );

    let mut headers = Map::new();
    for (name, value) in parse_extra_headers(&config.extra_headers)? {
        headers.insert(name, Value::String(value));
    }
    if !headers.is_empty() {
        root.insert("Headers".to_string(), Value::Object(headers));
    }

    root.insert(
        "Echo".to_string(),
        serde_json::to_value(config)
            .map_err(|err| format!("Failed to serialize Echo uploader settings: {err}"))?,
    );

    serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|err| format!("Failed to export uploader settings: {err}"))
}

/// Import an Echo or ShareX-style uploader JSON preset.
///
/// The top-level request fields are authoritative. Echo's extension object,
/// when present, restores the extra size/confirmation and clipboard behavior
/// controls. Presets without that extension enable both paste and drag/drop
/// entry points after successful validation.
pub fn import_uploader_config_json(
    json: &str,
    current: &ImageUploaderConfig,
) -> Result<ImageUploaderConfig, String> {
    if json.trim().is_empty() {
        return Err("Clipboard must not be empty".to_string());
    }

    let value: Value = serde_json::from_str(json)
        .map_err(|_| "Clipboard did not contain valid JSON".to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "Uploader JSON must be an object".to_string())?;

    if !object.contains_key("Version") {
        return Err("Uploader JSON must contain the 'Version' key".to_string());
    }

    let request_url = required_preset_string(object, "RequestURL")?;
    let form_field = required_preset_string(object, "FileFormName")?;
    let image_link = required_preset_string(object, "URL")?;

    let echo_extension = object.get("Echo");
    let mut config = if let Some(echo) = echo_extension {
        serde_json::from_value::<ImageUploaderConfig>(echo.clone())
            .map_err(|_| "Invalid Echo uploader extension".to_string())?
    } else {
        current.clone()
    };

    config.request_url = request_url.trim().to_string();
    config.form_field = form_field.trim().to_string();
    config.image_link_template = normalize_sharex_response_template(&image_link);
    config.deletion_link_template = object
        .get("DeletionURL")
        .and_then(Value::as_str)
        .map(normalize_sharex_response_template)
        .unwrap_or_default();
    config.extra_headers = import_preset_headers(object.get("Headers"))?;
    if echo_extension.is_none() {
        config.enabled = true;
        config.paste_upload_enabled = true;
        config.drag_drop_upload_enabled = true;
    }

    validate_config(&config)?;
    Ok(config)
}

fn required_preset_string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("Uploader JSON must contain the string '{key}' key"))
}

fn import_preset_headers(value: Option<&Value>) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "Uploader JSON 'Headers' must be an object".to_string())?;
    let mut headers = Vec::with_capacity(object.len());
    for (index, (name, value)) in object.iter().enumerate() {
        let value = value
            .as_str()
            .ok_or_else(|| format!("Uploader header {} must have a string value", index + 1))?;
        headers.push(format!("{name}: {value}"));
    }
    let headers = headers.join(" ; ");
    parse_extra_headers(&headers)?;
    Ok(headers)
}

fn normalize_sharex_response_template(template: &str) -> String {
    let template = template.trim();
    if template.eq_ignore_ascii_case("{response}") {
        return String::new();
    }

    let lower = template.to_ascii_lowercase();
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find("{json:") {
        let start = cursor + relative_start;
        output.push_str(&template[cursor..start]);

        let mut depth = 0usize;
        let mut end = None;
        for (relative, byte) in template.as_bytes()[start..].iter().copied().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(start + relative);
                        break;
                    }
                }
                _ => {}
            }
        }

        let Some(end) = end else {
            output.push_str(&template[start..]);
            return output;
        };

        let inner = template[start + "{json:".len()..end].trim();
        let path = inner
            .rsplit_once('|')
            .map_or(inner, |(_, path)| path.trim());
        output.push('{');
        output.push_str(&normalize_sharex_array_indices(path));
        output.push('}');
        cursor = end + 1;
    }
    output.push_str(&template[cursor..]);
    output
}

fn normalize_sharex_array_indices(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut output = String::with_capacity(path.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'['
            && let Some(relative_end) = bytes[cursor + 1..].iter().position(|byte| *byte == b']')
        {
            let end = cursor + 1 + relative_end;
            let index = &path[cursor + 1..end];
            if !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) {
                output.push('.');
                output.push_str(index);
                cursor = end + 1;
                continue;
            }
        }

        let character = path[cursor..]
            .chars()
            .next()
            .expect("cursor is inside the path");
        output.push(character);
        cursor += character.len_utf8();
    }
    output
}

pub fn validate_config(config: &ImageUploaderConfig) -> Result<(), String> {
    let request_url = config.request_url.trim();
    if request_url.is_empty() {
        if config.enabled {
            return Err("Request URL is required when image uploader is enabled".to_string());
        }
    } else {
        let parsed = url::Url::parse(request_url)
            .map_err(|_| "Request URL must be an absolute HTTP or HTTPS URL".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("Request URL must be an absolute HTTP or HTTPS URL".to_string());
        }
    }

    if config.enabled && config.form_field.trim().is_empty() {
        return Err("Form field is required".to_string());
    }

    parse_extra_headers(&config.extra_headers)?;
    Ok(())
}

/// Whether an uploader mapping still contains the account-preset API-key
/// placeholder. Callers must not send uploads with this mapping.
pub fn has_placeholder_uploader_key(config: &ImageUploaderConfig) -> bool {
    config
        .extra_headers
        .split(';')
        .filter_map(|item| item.split_once(':'))
        .any(|(_, value)| is_uploader_api_key_placeholder(value))
}

/// Whether an API-key field is the account-preset placeholder rather than a
/// usable key. Keep this comparison shared by settings normalization and the
/// transport boundary.
pub fn is_uploader_api_key_placeholder(value: &str) -> bool {
    value
        .trim()
        .eq_ignore_ascii_case(UPLOADER_API_KEY_PLACEHOLDER)
}

/// Parse extra headers written as `Header: value ; Next: value`.
pub fn parse_extra_headers(input: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();

    for (index, raw) in input.split(';').enumerate() {
        let index = index + 1;
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }

        let Some((name, value)) = item.split_once(':') else {
            return Err(format!("Header {index} must use 'Name: value' syntax"));
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return Err(format!("Header {index} must include both name and value"));
        }
        if name.contains('\r')
            || name.contains('\n')
            || value.contains('\r')
            || value.contains('\n')
        {
            return Err(format!("Header {index} cannot contain newline characters"));
        }

        HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Header {index} has an invalid name"))?;
        HeaderValue::from_str(value).map_err(|_| format!("Header {index} has an invalid value"))?;

        headers.push((name.to_string(), value.to_string()));
    }

    Ok(headers)
}

/// `extra_headers` with every value removed (`Name:`), for exports that must
/// not carry API keys. Items without a `:` are dropped, since they cannot be
/// split into a name and a value.
pub fn redact_extra_header_values(input: &str) -> String {
    input
        .split(';')
        .filter_map(|item| item.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{name}:"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `url` with every query value, bare query segment (`?token`), and any
/// user name or password replaced by `REDACTED`, and any fragment dropped.
/// Query keys stay, so the shape of an upload endpoint (`?key=...`) is still
/// visible while credentials passed in the URL are not.
pub fn redact_url_query_values(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url.trim()) else {
        return redact_query_text(url.split_once('#').map_or(url, |(head, _)| head));
    };
    parsed.set_fragment(None);
    if !parsed.username().is_empty() {
        let _ = parsed.set_username("REDACTED");
    }
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("REDACTED"));
    }
    if let Some(query) = parsed.query().map(redact_query_pairs) {
        parsed.set_query(Some(&query));
    }
    parsed.to_string()
}

/// Fallback for text that is not a URL: redact everything after the first `?`.
fn redact_query_text(text: &str) -> String {
    match text.split_once('?') {
        Some((base, query)) => format!("{base}?{}", redact_query_pairs(query)),
        None => text.to_string(),
    }
}

fn redact_query_pairs(query: &str) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) => format!("{key}=REDACTED"),
            None if pair.is_empty() => String::new(),
            None => "REDACTED".to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub fn parse_upload_response(
    response_text: &str,
    image_link_template: &str,
    deletion_link_template: &str,
) -> Result<ParsedUploadResponse, String> {
    let image_link = resolve_response_template(image_link_template, response_text)?
        .ok_or_else(|| "Upload response did not contain an image link".to_string())?;
    let image_link = validate_resolved_url(&image_link, "image")?;
    let deletion_link = resolve_response_template(deletion_link_template, response_text)?
        .and_then(|link| validate_resolved_url(&link, "deletion").ok());

    Ok(ParsedUploadResponse {
        image_link,
        deletion_link,
    })
}

/// Resolve an image/deletion link schema.
///
/// Empty schema means the server response body is the link. Non-empty schemas
/// can contain `{property}` placeholders with dot-notation for JSON responses.
pub fn resolve_response_template(
    template: &str,
    response_text: &str,
) -> Result<Option<String>, String> {
    let template = template.trim();
    if template.is_empty() {
        let body = response_text.trim();
        return if body.is_empty() {
            Ok(None)
        } else if let Ok(Value::String(value)) = serde_json::from_str::<Value>(body) {
            Ok(Some(value))
        } else {
            Ok(Some(body.to_string()))
        };
    }

    if !template.contains('{') {
        return Ok(Some(template.to_string()));
    }

    let json: Value = serde_json::from_str(response_text)
        .map_err(|_| "Response template requires JSON response".to_string())?;
    let mut output = String::with_capacity(template.len());
    let mut rest = template;

    loop {
        let Some(open) = rest.find('{') else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('}') else {
            return Err("Response template has an unclosed placeholder".to_string());
        };

        let path = after_open[..close].trim();
        if path.is_empty() {
            return Err("Response template contains an empty placeholder".to_string());
        }
        let value = json_path_to_string(&json, path)
            .ok_or_else(|| "Response JSON did not contain a configured field".to_string())?;
        output.push_str(&value);
        rest = &after_open[close + 1..];
    }

    Ok(Some(output))
}

pub fn mime_type_for_image_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

pub fn large_upload_confirmation_threshold(config: &ImageUploaderConfig) -> u64 {
    let half_limit = (config.max_upload_bytes / 2).max(1);
    if config.max_upload_bytes < MIN_LARGE_UPLOAD_CONFIRM_BYTES {
        config.max_upload_bytes.max(1)
    } else {
        half_limit
            .max(MIN_LARGE_UPLOAD_CONFIRM_BYTES)
            .min(config.max_upload_bytes)
    }
}

pub fn should_confirm_upload_size(config: &ImageUploaderConfig, bytes: u64) -> bool {
    config.ask_before_large_upload && bytes >= large_upload_confirmation_threshold(config)
}

pub fn format_upload_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;

    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB)
    }
}

pub fn upload_error_detail(response_text: &str) -> String {
    sanitize_and_truncate_detail(&extract_upload_error_detail(response_text))
}

/// Return an upload-server error detail with credentials configured for this
/// uploader removed before it can reach application status UI.
pub fn upload_error_detail_for_config(response_text: &str, config: &ImageUploaderConfig) -> String {
    let detail = redact_configured_credentials(&extract_upload_error_detail(response_text), config);
    sanitize_and_truncate_detail(&detail)
}

fn extract_upload_error_detail(response_text: &str) -> String {
    let structured = serde_json::from_str::<Value>(response_text)
        .ok()
        .map(|json| {
            ["code", "error", "message"]
                .into_iter()
                .filter_map(|field| {
                    find_json_scalar_field(&json, field, 0).map(|value| format!("{field}: {value}"))
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|detail| !detail.is_empty());

    structured.unwrap_or_else(|| response_text.to_string())
}

fn redact_configured_credentials(detail: &str, config: &ImageUploaderConfig) -> String {
    let request_url = config.request_url.trim();
    let redacted = detail.to_string();
    let mut secrets = configured_url_credentials(request_url);
    if !request_url.is_empty() {
        secrets.push(request_url.to_string());
    }
    if let Ok(url) = url::Url::parse(request_url) {
        secrets.extend(
            url.path_segments()
                .into_iter()
                .flatten()
                .filter(|segment| segment.len() >= 16)
                .map(ToString::to_string),
        );
    }
    secrets.extend(configured_header_values(&config.extra_headers));
    redact_exact_values(&redacted, &secrets)
}

fn configured_url_credentials(request_url: &str) -> Vec<String> {
    let Ok(url) = url::Url::parse(request_url) else {
        return Vec::new();
    };

    let mut values = Vec::new();
    add_raw_userinfo_variants(&mut values, request_url);
    add_secret_variants(&mut values, url.username());
    if let Some(password) = url.password() {
        add_secret_variants(&mut values, password);
    }
    if let Some(query) = url.query() {
        for pair in query.split('&') {
            if let Some((_, value)) = pair.split_once('=') {
                add_secret_variants(&mut values, value);
            }
        }
    }
    values.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
    values
}

fn add_raw_userinfo_variants(values: &mut Vec<String>, request_url: &str) {
    let Some((_, remainder)) = request_url.split_once("://") else {
        return;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or(remainder);
    let Some((userinfo, _)) = authority.rsplit_once('@') else {
        return;
    };
    let (username, password) = userinfo
        .split_once(':')
        .map_or((userinfo, None), |(user, pass)| (user, Some(pass)));
    add_secret_variants(values, username);
    if let Some(password) = password {
        add_secret_variants(values, password);
    }
}

fn configured_header_values(headers: &str) -> Vec<String> {
    let mut values = Vec::new();
    for value in headers
        .split(';')
        .filter_map(|item| item.split_once(':').map(|(_, value)| value.trim()))
        .filter(|value| !value.is_empty())
    {
        values.push(value.to_string());
        values.extend(
            value
                .split_whitespace()
                .filter(|segment| segment.len() >= 4)
                .map(ToString::to_string),
        );
    }
    values
}

fn add_secret_variants(values: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        return;
    }
    values.push(value.to_string());
    let decoded = percent_decode_preserving_plus(value);
    if decoded != value {
        values.push(decoded);
    }
}

fn percent_decode_preserving_plus(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[cursor + 1]), hex_value(bytes[cursor + 2]))
        {
            output.push(high << 4 | low);
            cursor += 3;
        } else {
            output.push(bytes[cursor]);
            cursor += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Replace configured values without ever scanning an inserted marker. Values
/// shorter than four bytes are too likely to be ordinary prose or identifiers
/// in a server error, so URL shape redaction covers them instead.
fn redact_exact_values(detail: &str, values: &[String]) -> String {
    const MIN_REDACTED_VALUE_BYTES: usize = 4;
    const MARKER: &str = "<redacted>";

    let mut values = values
        .iter()
        .filter(|value| value.len() >= MIN_REDACTED_VALUE_BYTES)
        .map(String::as_str)
        .collect::<Vec<_>>();
    values
        .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    values.dedup();

    let mut spans = Vec::new();
    for value in values {
        spans.extend(
            detail
                .match_indices(value)
                .map(|(start, _)| (start, start + value.len())),
        );
    }
    spans.sort_unstable_by(|(left_start, left_end), (right_start, right_end)| {
        left_start
            .cmp(right_start)
            .then_with(|| right_end.cmp(left_end))
    });

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if let Some(previous) = merged.last_mut()
            && start <= previous.1
        {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut output = String::with_capacity(detail.len());
    let mut cursor = 0;
    for (start, end) in merged {
        output.push_str(&detail[cursor..start]);
        output.push_str(MARKER);
        cursor = end;
    }
    output.push_str(&detail[cursor..]);
    output
}

fn find_json_scalar_field(json: &Value, field: &str, depth: usize) -> Option<String> {
    const MAX_SEARCH_DEPTH: usize = 4;

    if depth > MAX_SEARCH_DEPTH {
        return None;
    }

    match json {
        Value::Object(object) => {
            if let Some(value) = object
                .iter()
                .find_map(|(key, value)| key.eq_ignore_ascii_case(field).then_some(value))
                .and_then(json_scalar_to_string)
            {
                return Some(value);
            }

            object
                .values()
                .find_map(|value| find_json_scalar_field(value, field, depth + 1))
        }
        Value::Array(array) => array
            .iter()
            .find_map(|value| find_json_scalar_field(value, field, depth + 1)),
        _ => None,
    }
}

fn json_scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn sanitize_and_truncate_detail(detail: &str) -> String {
    const ELLIPSIS: &str = "...";
    let detail = redact_detail_urls(detail);
    let content_limit = MAX_UPLOAD_ERROR_DETAIL_BYTES.saturating_sub(ELLIPSIS.len());
    let mut sanitized = String::with_capacity(detail.len().min(MAX_UPLOAD_ERROR_DETAIL_BYTES));
    let mut pending_space = false;
    let mut truncated = false;

    for character in detail.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }

        let separator_bytes = usize::from(pending_space);
        if sanitized.len() + separator_bytes + character.len_utf8() > content_limit {
            truncated = true;
            break;
        }
        if pending_space {
            sanitized.push(' ');
            pending_space = false;
        }
        sanitized.push(character);
    }

    if truncated {
        sanitized.push_str(ELLIPSIS);
    }
    sanitized
}

fn redact_detail_urls(detail: &str) -> String {
    detail
        .split_whitespace()
        .map(|token| {
            let trimmed = token.trim_start_matches(|character| {
                matches!(character, '\'' | '"' | '(' | '[' | '{' | '<')
            });
            let core = trimmed.trim_end_matches(|character| {
                matches!(
                    character,
                    '\'' | '"' | ')' | ']' | '}' | '>' | ',' | '.' | ';' | ':'
                )
            });
            let prefix_len = token.len() - trimmed.len();
            let suffix = &trimmed[core.len()..];
            let (identifier, prefixed_url) = core
                .strip_prefix("ident=")
                .map_or(("", core), |url| ("ident=", url));
            let inner = prefixed_url.trim_start_matches(['\'', '"', '(', '[', '{', '<']);
            let inner_prefix = &prefixed_url[..prefixed_url.len() - inner.len()];
            let url_core = inner.trim_end_matches(['\'', '"', ')', ']', '}', '>']);
            let inner_suffix = &inner[url_core.len()..];
            url::Url::parse(url_core)
                .ok()
                .filter(|url| !url.cannot_be_a_base())
                .map_or_else(
                    || token.to_string(),
                    |_| {
                        format!(
                            "{}{}{}{}{}{}",
                            &token[..prefix_len],
                            identifier,
                            inner_prefix,
                            redact_url_query_values(url_core),
                            inner_suffix,
                            suffix
                        )
                    },
                )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_resolved_url(value: &str, kind: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("Upload response contained an empty {kind} URL"));
    }

    let parsed = url::Url::parse(value)
        .map_err(|_| format!("Upload response contained an invalid {kind} URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!(
            "Upload response {kind} URL must be an absolute HTTP or HTTPS URL"
        ));
    }

    Ok(value.to_string())
}

fn json_path_to_string(json: &Value, path: &str) -> Option<String> {
    let mut cursor = json;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        cursor = match cursor {
            Value::Object(object) => object.get(segment)?,
            Value::Array(array) => array.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }

    match cursor {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_query_values_keeps_keys_only() {
        assert_eq!(
            redact_url_query_values("https://up.example/api?key=secret&v=2#frag"),
            "https://up.example/api?key=REDACTED&v=REDACTED"
        );
        assert_eq!(
            redact_url_query_values("https://up.example/api?secrettoken"),
            "https://up.example/api?REDACTED"
        );
        assert_eq!(
            redact_url_query_values("https://user:pass@up.example/api?a=1&&tok"),
            "https://REDACTED:REDACTED@up.example/api?a=REDACTED&&REDACTED"
        );
        assert_eq!(
            redact_url_query_values("https://user@up.example/api"),
            "https://REDACTED@up.example/api"
        );
        assert_eq!(
            redact_url_query_values("not a url?key=v&tok#frag"),
            "not a url?key=REDACTED&REDACTED"
        );
        assert_eq!(
            redact_url_query_values(KAPPA_UPLOAD_URL),
            KAPPA_UPLOAD_URL.to_string()
        );
        let debug = format!(
            "{:?}",
            ImageUploaderConfig {
                request_url: "https://up.example/?token=abc".to_string(),
                ..ImageUploaderConfig::default()
            }
        );
        assert!(!debug.contains("abc"));
    }

    #[test]
    fn parses_semicolon_separated_header_list() {
        let parsed =
            parse_extra_headers("Authorization: Basic abc ; X-Test: value:with:colon").unwrap();
        assert_eq!(
            parsed,
            vec![
                ("Authorization".to_string(), "Basic abc".to_string()),
                ("X-Test".to_string(), "value:with:colon".to_string())
            ]
        );
    }

    #[test]
    fn malformed_header_errors_do_not_expose_values() {
        let canary = "super-secret-header-value";
        let missing_separator = parse_extra_headers(canary).unwrap_err();
        assert_eq!(missing_separator, "Header 1 must use 'Name: value' syntax");
        assert!(!missing_separator.contains(canary));

        let invalid_value =
            parse_extra_headers(&format!("Authorization: {canary}\u{7f}")).unwrap_err();
        assert_eq!(invalid_value, "Header 1 has an invalid value");
        assert!(!invalid_value.contains(canary));
    }

    #[test]
    fn disabled_config_validates_present_endpoint_and_headers() {
        let blank_disabled = ImageUploaderConfig {
            enabled: false,
            request_url: String::new(),
            form_field: String::new(),
            extra_headers: String::new(),
            ..ImageUploaderConfig::default()
        };
        assert!(validate_config(&blank_disabled).is_ok());

        let disabled_ftp = ImageUploaderConfig {
            enabled: false,
            request_url: "ftp://upload.example.test/file".to_string(),
            ..ImageUploaderConfig::default()
        };
        assert_eq!(
            validate_config(&disabled_ftp).unwrap_err(),
            "Request URL must be an absolute HTTP or HTTPS URL"
        );

        let disabled_bad_headers = ImageUploaderConfig {
            enabled: false,
            extra_headers: "Authorization Bearer super-secret-header-value".to_string(),
            ..ImageUploaderConfig::default()
        };
        let error = validate_config(&disabled_bad_headers).unwrap_err();
        assert_eq!(error, "Header 1 must use 'Name: value' syntax");
        assert!(!error.contains("super-secret-header-value"));
    }

    #[test]
    fn invalid_request_url_errors_do_not_expose_credentials() {
        let canary = "super-secret-url-password";
        let config = ImageUploaderConfig {
            enabled: false,
            request_url: format!("https://user:{canary}@"),
            ..ImageUploaderConfig::default()
        };
        let error = validate_config(&config).unwrap_err();
        assert_eq!(error, "Request URL must be an absolute HTTP or HTTPS URL");
        assert!(!error.contains(canary));
    }

    #[test]
    fn template_and_import_errors_do_not_expose_configured_values() {
        let canary = "super-secret-template-value";
        let error =
            resolve_response_template(&format!("https://user:{canary}@example.test/{{"), "{}")
                .unwrap_err();
        assert_eq!(error, "Response template has an unclosed placeholder");
        assert!(!error.contains(canary));

        let json = format!(
            r#"{{"Version":"1","RequestURL":"https://upload.example.test","FileFormName":"file","URL":"{{link}}","Headers":{{"Bad\n{canary}":"value"}}}}"#
        );
        let error =
            import_uploader_config_json(&json, &ImageUploaderConfig::default()).unwrap_err();
        assert_eq!(error, "Header 1 cannot contain newline characters");
        assert!(!error.contains(canary));

        let echo_json = format!(
            r#"{{"Version":"1","RequestURL":"https://upload.example.test","FileFormName":"file","URL":"{{link}}","Echo":{{"max_upload_bytes":"{canary}"}}}}"#
        );
        let error =
            import_uploader_config_json(&echo_json, &ImageUploaderConfig::default()).unwrap_err();
        assert_eq!(error, "Invalid Echo uploader extension");
        assert!(!error.contains(canary));

        let error = import_uploader_config_json(
            &format!(r#"{{"invalid":"{canary}""#),
            &ImageUploaderConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error, "Clipboard did not contain valid JSON");
        assert!(!error.contains(canary));

        let error =
            resolve_response_template("{link}", &format!(r#"{{"broken":"{canary}""#)).unwrap_err();
        assert_eq!(error, "Response template requires JSON response");
        assert!(!error.contains(canary));
    }

    #[test]
    fn upload_error_detail_redacts_url_credentials() {
        let detail = upload_error_detail(
            r#"{"message":"Request to https://user:super-secret-password@example.test/upload?token=super-secret-token failed"}"#,
        );
        assert!(!detail.contains("super-secret-password"));
        assert!(!detail.contains("super-secret-token"));
        assert!(detail.contains("REDACTED"));
    }

    #[test]
    fn configured_upload_error_detail_redacts_json_and_plain_credentials() {
        let config = ImageUploaderConfig {
            request_url: "https://user:super-secret-url-password@upload.example.test/path?token=super-secret-url-token".to_string(),
            extra_headers: "aUtHoRiZaTiOn: bEaReR super-secret-bearer-token ; X-API-Key: super-secret-api-key ; Cookie: super-secret-cookie ; X-Auth-Signature: super-secret-signature ; X-Session: super-secret-session ; X-Upload-Pass: super-secret-upload-pass ; Sid: super-secret-sid".to_string(),
            ..ImageUploaderConfig::default()
        };
        for response in [
            r#"{"message":"Rejected super-secret-bearer-token, super-secret-api-key, super-secret-cookie, super-secret-signature, super-secret-session, super-secret-upload-pass, super-secret-sid, super-secret-url-password, and super-secret-url-token at https://user:super-secret-url-password@upload.example.test/path?token=super-secret-url-token"}"#,
            "Rejected super-secret-bearer-token, super-secret-api-key, super-secret-cookie, super-secret-signature, super-secret-session, super-secret-upload-pass, super-secret-sid, super-secret-url-password, and super-secret-url-token at https://user:super-secret-url-password@upload.example.test/path?token=super-secret-url-token",
        ] {
            let detail = upload_error_detail_for_config(response, &config);
            for secret in [
                "super-secret-bearer-token",
                "super-secret-api-key",
                "super-secret-cookie",
                "super-secret-signature",
                "super-secret-session",
                "super-secret-upload-pass",
                "super-secret-sid",
                "super-secret-url-password",
                "super-secret-url-token",
            ] {
                assert!(!detail.contains(secret), "detail leaked {secret}");
            }
            assert!(detail.contains("Rejected"));
            assert!(detail.contains("<redacted>"));
        }
    }

    #[test]
    fn configured_detail_redaction_covers_header_segments_and_url_variants() {
        let config = ImageUploaderConfig {
            request_url: "https://user%2Dname:pass%2Bword@upload.example.test/api/webhooks/1234567890123456/webhook-token-12345678?token=query%2Dtoken%2Bvalue".to_string(),
            extra_headers: "X-Session: Token session-part-12345 ; X-Webhook: webhook-header-12345678".to_string(),
            ..ImageUploaderConfig::default()
        };
        let detail = upload_error_detail_for_config(
            "Rejected Token session-part-12345, user-name, pass+word, query-token+value, user%2Dname, pass%2Bword, query%2Dtoken%2Bvalue, /api/webhooks/1234567890123456/webhook-token-12345678, and webhook-header-12345678",
            &config,
        );
        for canary in [
            "session-part-12345",
            "user-name",
            "pass+word",
            "query-token+value",
            "user%2Dname",
            "pass%2Bword",
            "query%2Dtoken%2Bvalue",
            "1234567890123456",
            "webhook-token-12345678",
            "webhook-header-12345678",
        ] {
            assert!(!detail.contains(canary), "detail leaked {canary}");
        }
    }

    #[test]
    fn configured_detail_redaction_prefers_long_values_without_rescanning_markers() {
        let config = ImageUploaderConfig {
            extra_headers: "X-Session: session-token-1234 ; X-Other: token-1234".to_string(),
            ..ImageUploaderConfig::default()
        };
        let detail = upload_error_detail_for_config(
            "session-token-1234 was rejected after token-1234; <redacted> stays a marker",
            &config,
        );
        assert_eq!(
            detail,
            "<redacted> was rejected after <redacted>; <redacted> stays a marker"
        );
    }

    #[test]
    fn detail_redaction_keeps_short_ordinary_values_and_wrapped_urls_safe() {
        let detail = upload_error_detail(
            "Error 1: invalid json from (https://user:password@example.test/upload?token=secret) <https://name:pass@example.test/path?key=value>",
        );
        assert!(detail.contains("Error 1: invalid json"));
        assert!(!detail.contains("password"));
        assert!(!detail.contains("secret"));
        assert!(!detail.contains("pass"));
        assert!(!detail.contains("value"));
        assert!(detail.contains("(https://REDACTED:REDACTED@example.test/upload?token=REDACTED)"));
        assert!(detail.contains("<https://REDACTED:REDACTED@example.test/path?key=REDACTED>"));

        let identified = upload_error_detail(
            "provider returned ident=https://user:password@example.test/upload?token=secret",
        );
        assert!(
            identified
                .contains("ident=https://REDACTED:REDACTED@example.test/upload?token=REDACTED")
        );
    }

    #[test]
    fn recognizes_placeholder_uploader_key() {
        let config = ImageUploaderConfig {
            extra_headers: format!("X-API-Key: {UPLOADER_API_KEY_PLACEHOLDER}"),
            ..ImageUploaderConfig::default()
        };
        assert!(has_placeholder_uploader_key(&config));
        assert!(!has_placeholder_uploader_key(
            &ImageUploaderConfig::default()
        ));
        assert!(is_uploader_api_key_placeholder("  your_api_key_here  "));
        assert!(!is_uploader_api_key_placeholder("YOUR_API_KEY_HERE-actual"));
    }

    #[test]
    fn identifier_prefix_keeps_wrapped_url_redaction() {
        let detail = upload_error_detail(
            "ident=<https://user:unknown-password@example.test/path?key=unknown-query>",
        );
        assert!(!detail.contains("unknown-password"));
        assert!(!detail.contains("unknown-query"));
        assert!(detail.starts_with("ident=<https://"));
    }

    #[test]
    fn overlapping_configured_values_are_fully_redacted() {
        let config = ImageUploaderConfig {
            extra_headers: "X-Session: ABCDEF ; Sid: CDEFGHI".into(),
            ..ImageUploaderConfig::default()
        };
        let detail = upload_error_detail_for_config("Rejected ABCDEFGHI", &config);
        assert_eq!(detail, "Rejected <redacted>");
    }

    #[test]
    fn redacts_header_values_but_keeps_names() {
        assert_eq!(
            redact_extra_header_values("Authorization: Basic abc ; X-Test: v:w ; junk"),
            "Authorization:; X-Test:"
        );
        assert_eq!(redact_extra_header_values(""), "");
    }

    #[test]
    fn resolves_dot_notation_templates() {
        let response = r#"{"url":"https://i.example/a.png","file":{"id":"abc"}}"#;
        let parsed =
            parse_upload_response(response, "{url}", "https://del.example/{file.id}").unwrap();
        assert_eq!(parsed.image_link, "https://i.example/a.png");
        assert_eq!(
            parsed.deletion_link,
            Some("https://del.example/abc".to_string())
        );
    }
}
