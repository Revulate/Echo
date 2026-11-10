//! Contract: image-uploader presets in `src/backend/image_uploader.rs` and
//! `crates/echo-link-core`: config validation and secret redaction, header and
//! response-template parsing, ShareX import, export round trip, PNG encoding and
//! size thresholds. `--preset <file>` also verifies a real preset and
//! `--live-upload` uploads with it.

mod backend {
    pub mod http {
        pub fn client() -> reqwest::Client {
            reqwest::Client::new()
        }

        pub enum LogCategory {
            Net,
        }

        pub async fn send_logged(
            _category: LogCategory,
            request: reqwest::RequestBuilder,
        ) -> reqwest::Result<reqwest::Response> {
            request.send().await
        }
    }
}

mod settings {
    pub fn get_ask_before_paste_upload() -> bool {
        true
    }
}

#[path = "../../../src/backend/image_uploader.rs"]
#[allow(dead_code)] // paste/drop gating reads live settings and is not checked here
mod image_uploader;

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};
use image_uploader::{
    ImageUploaderConfig, UPLOADER_API_KEY_PLACEHOLDER, encode_rgba_png,
    export_uploader_config_json, format_upload_size, has_placeholder_uploader_key,
    import_uploader_config_json, large_upload_confirmation_threshold, mime_type_for_image_path,
    parse_extra_headers, parse_upload_response, should_confirm_upload_size,
    upload_error_detail_for_config, upload_image_bytes, validate_config,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
struct Cli {
    preset: Option<PathBuf>,
    live_upload: bool,
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _client = backend::http::client();
    let cli = parse_cli().unwrap_or_else(|err| panic!("{err}"));

    assert_eq!(
        image_uploader::upload_thumbnail_url("https://olrite.lol/abc123"),
        "https://olrite.lol/thumb/abc123/400.webp"
    );
    assert_eq!(
        image_uploader::upload_thumbnail_url("https://uploads.example.test/image.png"),
        "https://uploads.example.test/image.png"
    );
    assert_eq!(
        image_uploader::upload_thumbnail_url("https://olrite.lol/thumb/abc123/400.webp"),
        "https://olrite.lol/thumb/abc123/400.webp"
    );

    let headers = "Authorization: Basic abc123 ; X-Upload-Token: token:value";
    let response = r#"{"url":"https://i.example.test/a.png","deleteUrl":"https://i.example.test/delete/a","file":{"name":"a.png","id":"abc"}}"#;

    let parsed_headers = parse_extra_headers(headers).expect("headers should parse");
    assert_eq!(parsed_headers.len(), 2);
    assert!(parse_extra_headers("BrokenHeader").is_err());
    assert_eq!(
        mime_type_for_image_path(Path::new("drop.PNG")),
        Some("image/png")
    );
    assert_eq!(mime_type_for_image_path(Path::new("drop.txt")), None);

    let parsed = parse_upload_response(response, "{url}", "https://delete.example.test/{file.id}")
        .expect("response template should resolve");
    assert_eq!(parsed.image_link, "https://i.example.test/a.png");
    assert_eq!(
        parsed.deletion_link.as_deref(),
        Some("https://delete.example.test/abc")
    );

    let config = ImageUploaderConfig {
        enabled: true,
        request_url: "https://i.example.test/upload".to_string(),
        form_field: "file".to_string(),
        extra_headers: headers.to_string(),
        image_link_template: "{url}".to_string(),
        deletion_link_template: "{deleteUrl}".to_string(),
        ..ImageUploaderConfig::default()
    };
    validate_config(&config).expect("config should validate");
    let error_detail = upload_error_detail_for_config(
        "Authorization: Basic abc123 was rejected at https://user:password@i.example.test/upload?token=fixture",
        &config,
    );
    assert!(!error_detail.contains("Basic abc123"));
    assert!(!error_detail.contains("password"));
    assert!(!error_detail.contains("fixture"));
    assert!(has_placeholder_uploader_key(&ImageUploaderConfig {
        extra_headers: format!("X-API-Key: {UPLOADER_API_KEY_PLACEHOLDER}"),
        ..ImageUploaderConfig::default()
    }));
    assert_eq!(format_upload_size(1536), "1.5 KB");
    assert_eq!(
        large_upload_confirmation_threshold(&config),
        50 * 1024 * 1024
    );
    assert!(!should_confirm_upload_size(&config, 50 * 1024 * 1024 - 1));
    assert!(should_confirm_upload_size(&config, 50 * 1024 * 1024));
    let invalid_config = ImageUploaderConfig {
        enabled: true,
        request_url: String::new(),
        ..ImageUploaderConfig::default()
    };
    assert!(validate_config(&invalid_config).is_err());
    let disabled_blank_config = ImageUploaderConfig {
        enabled: false,
        request_url: String::new(),
        form_field: String::new(),
        extra_headers: String::new(),
        ..ImageUploaderConfig::default()
    };
    assert!(validate_config(&disabled_blank_config).is_ok());
    let disabled_ftp_config = ImageUploaderConfig {
        enabled: false,
        request_url: "ftp://upload.example.test/file".to_string(),
        ..ImageUploaderConfig::default()
    };
    assert!(validate_config(&disabled_ftp_config).is_err());
    let header_canary = "super-secret-header-value";
    let disabled_bad_headers = ImageUploaderConfig {
        enabled: false,
        extra_headers: format!("Authorization Bearer {header_canary}"),
        ..ImageUploaderConfig::default()
    };
    let header_error = validate_config(&disabled_bad_headers).unwrap_err();
    assert!(!header_error.contains(header_canary));
    let rgba = make_rgba_image(128, 128);
    let encoded = encode_rgba_png(128, 128, rgba).expect("RGBA should encode as PNG");
    assert!(encoded.starts_with(b"\x89PNG"));

    let exported = export_uploader_config_json(&config).expect("config should export");
    let round_trip = import_uploader_config_json(&exported, &ImageUploaderConfig::default())
        .expect("exported config should import");
    assert_eq!(round_trip, config);

    let sharex = r#"{
        "Version":"14.0.0",
        "RequestMethod":"POST",
        "RequestURL":"https://share.example.test/upload",
        "Body":"MultipartFormData",
        "FileFormName":"upload",
        "Headers":{"Authorization":"Bearer fixture","X-Key":"value:with:colon"},
        "URL":"https://cdn.example.test/{json:{response}|files[0].url}",
        "DeletionURL":"{json:delete.url}"
    }"#;
    let sharex_base = ImageUploaderConfig {
        max_upload_bytes: 32 * 1024 * 1024,
        ..ImageUploaderConfig::default()
    };
    let sharex_config =
        import_uploader_config_json(sharex, &sharex_base).expect("ShareX preset should import");
    assert!(sharex_config.enabled);
    assert!(sharex_config.paste_upload_enabled);
    assert!(sharex_config.drag_drop_upload_enabled);
    assert_eq!(sharex_config.max_upload_bytes, 32 * 1024 * 1024);
    assert_eq!(
        sharex_config.image_link_template,
        "https://cdn.example.test/{files.0.url}"
    );
    assert_eq!(sharex_config.deletion_link_template, "{delete.url}");
    assert_eq!(
        parse_extra_headers(&sharex_config.extra_headers)
            .unwrap()
            .len(),
        2
    );
    assert!(import_uploader_config_json("{}", &sharex_base).is_err());

    println!("{FIXTURE_HEADER}");
    print_smoke_rows(headers, response, &config, &encoded);

    if let Some(path) = cli.preset.as_deref() {
        verify_preset(path, cli.live_upload)
            .await
            .unwrap_or_else(|err| panic!("Uploader preset verification failed: {err}"));
    }
}

fn parse_cli() -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--preset" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--preset requires a path".to_string())?;
                cli.preset = Some(PathBuf::from(path));
            }
            "--live-upload" => cli.live_upload = true,
            "--help" | "-h" => {
                println!("Usage: upload-check [--preset <uploader.json>] [--live-upload]");
                std::process::exit(0);
            }
            other => return Err(format!("Unknown argument: {other}")),
        }
    }
    if cli.live_upload && cli.preset.is_none() {
        return Err("--live-upload requires --preset".to_string());
    }
    Ok(cli)
}

async fn verify_preset(path: &Path, live_upload: bool) -> Result<(), String> {
    let preset_text =
        fs::read_to_string(path).map_err(|err| format!("Could not read uploader preset: {err}"))?;
    let imported = import_uploader_config_json(&preset_text, &ImageUploaderConfig::default())?;
    validate_config(&imported)?;
    let parsed_headers = parse_extra_headers(&imported.extra_headers)?;

    let exported = export_uploader_config_json(&imported)?;
    let round_trip = import_uploader_config_json(&exported, &ImageUploaderConfig::default())?;
    if round_trip != imported {
        return Err("Echo portable export/import did not preserve the imported config".into());
    }

    let endpoint = url::Url::parse(&imported.request_url)
        .map_err(|err| format!("Imported endpoint URL was invalid: {err}"))?;
    println!(
        "exact,preset_import,passed=true,enabled={},paste={},drag_drop={},ask_before_large={},headers={},endpoint_scheme={},endpoint_host={},image_template=true,deletion_template=true,secret_redacted=true",
        imported.enabled,
        imported.paste_upload_enabled,
        imported.drag_drop_upload_enabled,
        imported.ask_before_large_upload,
        parsed_headers.len(),
        endpoint.scheme(),
        endpoint.host_str().unwrap_or("none")
    );
    println!("exact,portable_round_trip,passed=true,echo_extension=true,secret_redacted=true");

    if !live_upload {
        return Ok(());
    }
    if !imported.enabled {
        return Err("Uploader preset is disabled; refusing live upload".into());
    }

    let png = encode_rgba_png(8, 8, make_rgba_image(8, 8))?;
    let result = upload_image_bytes(
        &imported,
        "echo-uploader-verification.png",
        "image/png",
        png,
    )
    .await?;
    let image_url = url::Url::parse(&result.image_link)
        .map_err(|err| format!("Live image link was invalid: {err}"))?;
    let deletion_url = result
        .deletion_link
        .as_deref()
        .ok_or_else(|| {
            "Live upload response did not include the configured deletion link".to_string()
        })
        .and_then(|value| {
            url::Url::parse(value).map_err(|err| format!("Live deletion link was invalid: {err}"))
        })?;

    let response = backend::http::client()
        .get(result.image_link)
        .send()
        .await
        .map_err(|err| format!("Could not fetch the uploaded image: {err}"))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("Could not read the uploaded image: {err}"))?;
    let png_signature = body.starts_with(b"\x89PNG\r\n\x1a\n");
    if !status.is_success() || body.is_empty() {
        return Err(format!(
            "Uploaded share link was not usable: status={status}, body_empty={}",
            body.is_empty()
        ));
    }

    println!(
        "live,upload,passed=true,image_scheme={},image_host={},deletion_scheme={},deletion_host={},fetch_status={},body_nonempty=true,png_signature={},image_content_type={},secret_redacted=true",
        image_url.scheme(),
        image_url.host_str().unwrap_or("none"),
        deletion_url.scheme(),
        deletion_url.host_str().unwrap_or("none"),
        status.as_u16(),
        png_signature,
        content_type.starts_with("image/")
    );
    Ok(())
}

fn print_smoke_rows(
    headers: &str,
    response: &str,
    config: &ImageUploaderConfig,
    encoded_png: &[u8],
) {
    let parsed_headers = parse_extra_headers(headers).expect("headers should parse");
    assert_fixture("upload_header_count", parsed_headers.len().to_string(), "2");
    assert_fixture(
        "upload_header_authorization",
        format!("{}={}", parsed_headers[0].0, parsed_headers[0].1),
        "Authorization=Basic abc123",
    );
    assert_fixture(
        "upload_header_token_colon_value",
        format!("{}={}", parsed_headers[1].0, parsed_headers[1].1),
        "X-Upload-Token=token:value",
    );
    assert_fixture(
        "upload_header_missing_colon_rejected",
        bool_text(parse_extra_headers("BrokenHeader").is_err()),
        "true",
    );

    assert_fixture(
        "upload_mime_png_uppercase",
        mime_type_for_image_path(Path::new("drop.PNG")).unwrap_or("none"),
        "image/png",
    );
    assert_fixture(
        "upload_mime_txt_rejected",
        bool_text(mime_type_for_image_path(Path::new("drop.txt")).is_none()),
        "true",
    );

    let parsed = parse_upload_response(response, "{url}", "https://delete.example.test/{file.id}")
        .expect("response template should resolve");
    assert_fixture(
        "upload_response_image_link",
        &parsed.image_link,
        "https://i.example.test/a.png",
    );
    assert_fixture(
        "upload_response_delete_link",
        parsed.deletion_link.as_deref().unwrap_or("none"),
        "https://delete.example.test/abc",
    );

    assert_fixture(
        "upload_config_valid",
        bool_text(validate_config(config).is_ok()),
        "true",
    );
    let error_detail = upload_error_detail_for_config(
        "Authorization: Basic abc123 was rejected at https://user:password@i.example.test/upload?token=fixture",
        config,
    );
    assert_fixture(
        "upload_configured_error_credentials_redacted",
        bool_text(
            !error_detail.contains("Basic abc123")
                && !error_detail.contains("password")
                && !error_detail.contains("fixture"),
        ),
        "true",
    );
    let redaction_config = ImageUploaderConfig {
        request_url: "https://user%2Dname:pass%2Bword@upload.example.test/api/webhooks/1234567890123456/webhook-token-12345678?token=query%2Dtoken%2Bvalue".to_string(),
        extra_headers: "X-Session: Token session-part-12345 ; X-Webhook: webhook-header-12345678 ; X-Bot: Bot bot-canary-12345 ; X-Client: Client-ID client-canary-12345 ; X-OAuth: OAuth oauth-canary-12345 ; X-Key: Key key-canary-12345".to_string(),
        ..ImageUploaderConfig::default()
    };
    let redaction_detail = upload_error_detail_for_config(
        "Rejected Token session-part-12345, bot-canary-12345, client-canary-12345, oauth-canary-12345, key-canary-12345, user-name, pass+word, query-token+value, user%2Dname, pass%2Bword, query%2Dtoken%2Bvalue, 1234567890123456, webhook-token-12345678, and webhook-header-12345678",
        &redaction_config,
    );
    assert_fixture(
        "upload_configured_redaction_canaries",
        bool_text(
            [
                "session-part-12345",
                "user-name",
                "pass+word",
                "query-token+value",
                "user%2Dname",
                "pass%2Bword",
                "query%2Dtoken%2Bvalue",
                "bot-canary-12345",
                "client-canary-12345",
                "oauth-canary-12345",
                "key-canary-12345",
                "1234567890123456",
                "webhook-token-12345678",
                "webhook-header-12345678",
            ]
            .into_iter()
            .all(|canary| !redaction_detail.contains(canary)),
        ),
        "true",
    );
    let identified_detail = upload_error_detail_for_config(
        "ident=<https://unknown-user:unknown-password@example.test/upload?key=unknown-query>",
        &redaction_config,
    );
    assert_fixture(
        "upload_identified_url_credentials_redacted",
        bool_text(
            identified_detail.starts_with("ident=<https://")
                && !identified_detail.contains("unknown-user")
                && !identified_detail.contains("unknown-password")
                && !identified_detail.contains("unknown-query"),
        ),
        "true",
    );
    assert_fixture(
        "upload_placeholder_key_detected",
        bool_text(has_placeholder_uploader_key(&ImageUploaderConfig {
            extra_headers: format!("X-API-Key: {UPLOADER_API_KEY_PLACEHOLDER}"),
            ..ImageUploaderConfig::default()
        })),
        "true",
    );
    let invalid_config = ImageUploaderConfig {
        enabled: true,
        request_url: String::new(),
        ..ImageUploaderConfig::default()
    };
    assert_fixture(
        "upload_config_missing_url_rejected",
        bool_text(validate_config(&invalid_config).is_err()),
        "true",
    );
    let disabled_blank_config = ImageUploaderConfig {
        enabled: false,
        request_url: String::new(),
        form_field: String::new(),
        extra_headers: String::new(),
        ..ImageUploaderConfig::default()
    };
    assert_fixture(
        "upload_disabled_blank_config_valid",
        bool_text(validate_config(&disabled_blank_config).is_ok()),
        "true",
    );
    let disabled_ftp_config = ImageUploaderConfig {
        enabled: false,
        request_url: "ftp://upload.example.test/file".to_string(),
        ..ImageUploaderConfig::default()
    };
    assert_fixture(
        "upload_disabled_ftp_rejected",
        bool_text(validate_config(&disabled_ftp_config).is_err()),
        "true",
    );
    let header_canary = "super-secret-header-value";
    let disabled_bad_headers = ImageUploaderConfig {
        enabled: false,
        extra_headers: format!("Authorization Bearer {header_canary}"),
        ..ImageUploaderConfig::default()
    };
    let header_error = validate_config(&disabled_bad_headers).unwrap_err();
    assert_fixture(
        "upload_invalid_header_redacted",
        bool_text(!header_error.contains(header_canary)),
        "true",
    );
    assert_fixture("upload_size_1536", format_upload_size(1536), "1.5 KB");
    assert_fixture(
        "upload_large_threshold",
        large_upload_confirmation_threshold(config).to_string(),
        "52428800",
    );
    assert_fixture(
        "upload_confirm_below_threshold",
        bool_text(should_confirm_upload_size(config, 50 * 1024 * 1024 - 1)),
        "false",
    );
    assert_fixture(
        "upload_confirm_at_threshold",
        bool_text(should_confirm_upload_size(config, 50 * 1024 * 1024)),
        "true",
    );
    assert_fixture(
        "upload_png_signature",
        bool_text(encoded_png.starts_with(b"\x89PNG")),
        "true",
    );

    let exported = export_uploader_config_json(config).expect("config should export");
    let round_trip = import_uploader_config_json(&exported, &ImageUploaderConfig::default())
        .expect("exported config should import");
    assert_fixture(
        "upload_portable_native_round_trip",
        bool_text(round_trip == *config),
        "true",
    );

    let sharex = r#"{
        "Version":"14.0.0",
        "RequestURL":"https://share.example.test/upload",
        "FileFormName":"upload",
        "Headers":{"Authorization":"Bearer fixture","X-Key":"value:with:colon"},
        "URL":"https://cdn.example.test/{json:{response}|files[0].url}",
        "DeletionURL":"{json:delete.url}"
    }"#;
    let sharex_base = ImageUploaderConfig {
        max_upload_bytes: 32 * 1024 * 1024,
        ..ImageUploaderConfig::default()
    };
    let imported =
        import_uploader_config_json(sharex, &sharex_base).expect("ShareX preset should import");
    assert_fixture(
        "upload_portable_sharex_template",
        &imported.image_link_template,
        "https://cdn.example.test/{files.0.url}",
    );
    assert_fixture(
        "upload_portable_sharex_deletion_template",
        &imported.deletion_link_template,
        "{delete.url}",
    );
    assert_fixture(
        "upload_portable_sharex_entrypoints_and_limit",
        bool_text(
            imported.enabled
                && imported.paste_upload_enabled
                && imported.drag_drop_upload_enabled
                && imported.max_upload_bytes == 32 * 1024 * 1024,
        ),
        "true",
    );
    assert_fixture(
        "upload_portable_missing_version_rejected",
        bool_text(import_uploader_config_json("{}", &sharex_base).is_err()),
        "true",
    );
}

fn make_rgba_image(width: usize, height: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.push((x % 256) as u8);
            rgba.push((y % 256) as u8);
            rgba.push(((x ^ y) % 256) as u8);
            rgba.push(255);
        }
    }
    rgba
}
