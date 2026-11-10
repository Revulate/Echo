use std::env;
use std::fs;
use std::path::Path;

#[derive(Debug)]
struct ProfileSummary {
    source: String,
    channel: String,
    duration_secs: u64,
    use_emote_atlas: bool,
    messages_received: u64,
    messages_rendered: u64,
    stored_messages: u64,
    app_render_count: u64,
    layout_cache_hits: u64,
    layout_cache_misses: u64,
    layout_async_requests: u64,
    layout_async_ready_valid_width: u64,
    layout_async_ready_stale_width: u64,
    layout_full_rebuilds: u64,
    layout_message_order_resets: u64,
    layout_rebuild_reason_order: u64,
    layout_rebuild_reason_height: u64,
    layout_rebuild_reason_stale: u64,
    layout_rebuild_reason_force: u64,
    first_badge_draw_ms: u64,
    first_emote_draw_ms: u64,
    atlas_pages_used: u64,
    atlas_resident_pages: u64,
    atlas_insertions: u64,
    atlas_evictions: u64,
    atlas_rebuilds: u64,
    atlas_scale_count: u64,
    atlas_scale_ms: f64,
    atlas_upload_count: u64,
    atlas_upload_ms: f64,
    atlas_texture_upload_bytes: u64,
    top_emote_stage: String,
    top_emote_stage_ms: f64,
    top_emote_stage_items: u64,
}

fn main() {
    let paths = env::args().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: profile-summary-check <profile.summary.json> [...]");
        std::process::exit(2);
    }

    println!(
        "source,channel,duration_secs,use_emote_atlas,messages_received,messages_rendered,stored_messages,app_render_count,layout_cache_hits,layout_cache_misses,layout_async_requests,layout_async_ready_valid_width,layout_async_ready_stale_width,layout_full_rebuilds,layout_message_order_resets,layout_rebuild_reason_order,layout_rebuild_reason_height,layout_rebuild_reason_stale,layout_rebuild_reason_force,first_badge_draw_ms,first_emote_draw_ms,atlas_pages_used,atlas_resident_pages,atlas_insertions,atlas_evictions,atlas_rebuilds,atlas_scale_count,atlas_scale_ms,atlas_upload_count,atlas_upload_ms,atlas_texture_upload_bytes,top_emote_stage,top_emote_stage_ms,top_emote_stage_items"
    );

    for path in paths {
        match parse_profile_summary(&path) {
            Ok(summary) => println!("{}", summary.to_csv_row()),
            Err(error) => {
                eprintln!("{path}: {error}");
                std::process::exit(1);
            }
        }
    }
}

impl ProfileSummary {
    fn to_csv_row(&self) -> String {
        [
            csv_field(&self.source),
            csv_field(&self.channel),
            self.duration_secs.to_string(),
            self.use_emote_atlas.to_string(),
            self.messages_received.to_string(),
            self.messages_rendered.to_string(),
            self.stored_messages.to_string(),
            self.app_render_count.to_string(),
            self.layout_cache_hits.to_string(),
            self.layout_cache_misses.to_string(),
            self.layout_async_requests.to_string(),
            self.layout_async_ready_valid_width.to_string(),
            self.layout_async_ready_stale_width.to_string(),
            self.layout_full_rebuilds.to_string(),
            self.layout_message_order_resets.to_string(),
            self.layout_rebuild_reason_order.to_string(),
            self.layout_rebuild_reason_height.to_string(),
            self.layout_rebuild_reason_stale.to_string(),
            self.layout_rebuild_reason_force.to_string(),
            self.first_badge_draw_ms.to_string(),
            self.first_emote_draw_ms.to_string(),
            self.atlas_pages_used.to_string(),
            self.atlas_resident_pages.to_string(),
            self.atlas_insertions.to_string(),
            self.atlas_evictions.to_string(),
            self.atlas_rebuilds.to_string(),
            self.atlas_scale_count.to_string(),
            format!("{:.6}", self.atlas_scale_ms),
            self.atlas_upload_count.to_string(),
            format!("{:.6}", self.atlas_upload_ms),
            self.atlas_texture_upload_bytes.to_string(),
            csv_field(&self.top_emote_stage),
            format!("{:.6}", self.top_emote_stage_ms),
            self.top_emote_stage_items.to_string(),
        ]
        .join(",")
    }
}

fn parse_profile_summary(path: &str) -> Result<ProfileSummary, String> {
    let source = fs::read_to_string(path).map_err(|error| format!("failed to read: {error}"))?;
    let atlas = object_value(&source, "atlas")?;
    let layout = optional_object_value(&source, "layout")?.unwrap_or_default();
    let memory = object_value(&source, "memory")?;
    let top_stage = first_array_object(&source, "top_emote_stages")?;

    let summary = ProfileSummary {
        source: Path::new(path)
            .to_str()
            .ok_or_else(|| "profile path is not valid UTF-8".to_string())?
            .to_string(),
        channel: string_value(&source, "channel")?,
        duration_secs: u64_value(&source, "duration_secs")?,
        use_emote_atlas: bool_value(&source, "use_emote_atlas")?,
        messages_received: u64_value(&source, "messages_received")?,
        messages_rendered: u64_value(&source, "messages_rendered")?,
        stored_messages: u64_value(&memory, "stored_messages")?,
        app_render_count: u64_value(&source, "app_render_count")?,
        layout_cache_hits: u64_optional_value(&layout, "cache_hits")?,
        layout_cache_misses: u64_optional_value(&layout, "cache_misses")?,
        layout_async_requests: u64_optional_value(&layout, "async_requests")?,
        layout_async_ready_valid_width: u64_optional_value(&layout, "async_ready_valid_width")?,
        layout_async_ready_stale_width: u64_optional_value(&layout, "async_ready_stale_width")?,
        layout_full_rebuilds: u64_optional_value(&layout, "full_rebuilds")?,
        layout_message_order_resets: u64_optional_value(&layout, "message_order_resets")?,
        layout_rebuild_reason_order: u64_optional_value(&layout, "rebuild_reason_order")?,
        layout_rebuild_reason_height: u64_optional_value(&layout, "rebuild_reason_height")?,
        layout_rebuild_reason_stale: u64_optional_value(&layout, "rebuild_reason_stale")?,
        layout_rebuild_reason_force: u64_optional_value(&layout, "rebuild_reason_force")?,
        first_badge_draw_ms: u64_value(&source, "first_badge_draw_ms")?,
        first_emote_draw_ms: u64_value(&source, "first_emote_draw_ms")?,
        atlas_pages_used: u64_value(&atlas, "pages_used")?,
        atlas_resident_pages: u64_value(&atlas, "resident_pages")?,
        atlas_insertions: u64_value(&atlas, "insertions")?,
        atlas_evictions: u64_value(&atlas, "evictions")?,
        atlas_rebuilds: u64_value(&atlas, "rebuilds")?,
        atlas_scale_count: u64_value(&atlas, "scale_count")?,
        atlas_scale_ms: ns_field_to_ms(&atlas, "scale_ns")?,
        atlas_upload_count: u64_value(&atlas, "upload_count")?,
        atlas_upload_ms: ns_field_to_ms(&atlas, "upload_ns")?,
        atlas_texture_upload_bytes: u64_last_value(&atlas, "texture_upload_bytes")?,
        top_emote_stage: string_value(&top_stage, "stage")?,
        top_emote_stage_ms: f64_value(&top_stage, "last_ms")?,
        top_emote_stage_items: u64_value(&top_stage, "last_items")?,
    };

    validate_summary(&summary)?;
    Ok(summary)
}

fn validate_summary(summary: &ProfileSummary) -> Result<(), String> {
    if summary.duration_secs == 0 {
        return Err("duration_secs must be non-zero".to_string());
    }
    if summary.app_render_count == 0 {
        return Err("app_render_count must be non-zero".to_string());
    }
    if summary.use_emote_atlas && summary.atlas_pages_used == 0 {
        return Err("atlas-enabled profile has zero atlas pages".to_string());
    }
    if summary.atlas_scale_count > 0 && summary.atlas_scale_ms <= 0.0 {
        return Err("atlas scale count is non-zero but scale time is zero".to_string());
    }
    if summary.atlas_upload_count > 0 && summary.atlas_upload_ms <= 0.0 {
        return Err("atlas upload count is non-zero but upload time is zero".to_string());
    }
    if summary.top_emote_stage.is_empty() {
        return Err("top emote stage is empty".to_string());
    }
    Ok(())
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn ns_field_to_ms(source: &str, name: &str) -> Result<f64, String> {
    Ok(f64_value(source, name)? / 1_000_000.0)
}

fn string_value(source: &str, name: &str) -> Result<String, String> {
    let value = raw_value(source, name)?;
    let value = value.trim_start();
    if !value.starts_with('"') {
        return Err(format!("{name} is not a string"));
    }

    let mut escaped = false;
    let mut output = String::new();
    for ch in value[1..].chars() {
        if escaped {
            output.push(match ch {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Ok(output);
        } else {
            output.push(ch);
        }
    }

    Err(format!("{name} string is unterminated"))
}

fn bool_value(source: &str, name: &str) -> Result<bool, String> {
    let value = raw_value(source, name)?.trim_start();
    if value.starts_with("true") {
        Ok(true)
    } else if value.starts_with("false") {
        Ok(false)
    } else {
        Err(format!("{name} is not a boolean"))
    }
}

fn u64_value(source: &str, name: &str) -> Result<u64, String> {
    number_token(source, name)?
        .parse::<u64>()
        .map_err(|error| format!("{name} is not an unsigned integer: {error}"))
}

fn u64_optional_value(source: &str, name: &str) -> Result<u64, String> {
    if source.is_empty() {
        return Ok(0);
    }
    if raw_value(source, name).is_err() {
        return Ok(0);
    }
    u64_value(source, name)
}

fn u64_last_value(source: &str, name: &str) -> Result<u64, String> {
    number_token_from(raw_last_value(source, name)?, name)?
        .parse::<u64>()
        .map_err(|error| format!("{name} is not an unsigned integer: {error}"))
}

fn f64_value(source: &str, name: &str) -> Result<f64, String> {
    number_token(source, name)?
        .parse::<f64>()
        .map_err(|error| format!("{name} is not a number: {error}"))
}

fn number_token(source: &str, name: &str) -> Result<String, String> {
    number_token_from(raw_value(source, name)?, name)
}

fn number_token_from(source: &str, name: &str) -> Result<String, String> {
    let value = source.trim_start();
    let end = value
        .find(|ch: char| !matches!(ch, '-' | '+' | '.' | '0'..='9' | 'e' | 'E'))
        .unwrap_or(value.len());
    if end == 0 {
        return Err(format!("{name} numeric value is empty"));
    }
    Ok(value[..end].to_string())
}

fn object_value(source: &str, name: &str) -> Result<String, String> {
    let value = raw_value(source, name)?.trim_start();
    if !value.starts_with('{') {
        return Err(format!("{name} is not an object"));
    }
    balanced_slice(value, '{', '}').ok_or_else(|| format!("{name} object is unterminated"))
}

fn optional_object_value(source: &str, name: &str) -> Result<Option<String>, String> {
    let Ok(value) = raw_value(source, name) else {
        return Ok(None);
    };
    let value = value.trim_start();
    if !value.starts_with('{') {
        return Err(format!("{name} is not an object"));
    }
    balanced_slice(value, '{', '}')
        .map(Some)
        .ok_or_else(|| format!("{name} object is unterminated"))
}

fn first_array_object(source: &str, name: &str) -> Result<String, String> {
    let value = raw_value(source, name)?.trim_start();
    let array_start = value
        .find('[')
        .ok_or_else(|| format!("{name} is not an array"))?;
    let after_array = value[array_start + 1..].trim_start();
    if !after_array.starts_with('{') {
        return Err(format!("{name} first entry is not an object"));
    }
    balanced_slice(after_array, '{', '}')
        .ok_or_else(|| format!("{name} first object is unterminated"))
}

fn balanced_slice(source: &str, open: char, close: char) -> Option<String> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in source.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
        } else if ch == open {
            depth = depth.saturating_add(1);
        } else if ch == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(source[..=idx].to_string());
            }
        }
    }

    None
}

fn raw_value<'a>(source: &'a str, name: &str) -> Result<&'a str, String> {
    let needle = format!("\"{name}\"");
    let start = source
        .find(&needle)
        .ok_or_else(|| format!("missing field {name}"))?
        + needle.len();
    let after_name = &source[start..];
    let colon = after_name
        .find(':')
        .ok_or_else(|| format!("field {name} has no colon"))?;
    Ok(&after_name[colon + 1..])
}

fn raw_last_value<'a>(source: &'a str, name: &str) -> Result<&'a str, String> {
    let needle = format!("\"{name}\"");
    let start = source
        .rfind(&needle)
        .ok_or_else(|| format!("missing field {name}"))?
        + needle.len();
    let after_name = &source[start..];
    let colon = after_name
        .find(':')
        .ok_or_else(|| format!("field {name} has no colon"))?;
    Ok(&after_name[colon + 1..])
}
