//! JSON theme file parsing.
//!
//! Nested theme files (`window`, `splits`, `messages`, `tabs`, ... color groups)
//! have a large schema. Echo maps their stable chat-window color surface into
//! its smaller palette while preserving a compact flat schema for Echo-native
//! theme files.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub const ECHO_THEME_SCHEMA_URL: &str = "https://echo.local/EchoTheme.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeFilePalette {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    pub bg_void: String,
    pub bg_surface: String,
    pub bg_plate: String,
    pub bg_elevated: String,
    pub bg_hover: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub border: String,
    pub border_strong: String,
}

pub fn parse_theme_file_json(input: &str) -> Result<ThemeFilePalette, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("Invalid theme JSON: {e}"))?;
    parse_theme_value(&value)
}

pub fn parse_theme_file(path: &Path) -> Result<ThemeFilePalette, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read theme file '{}': {e}", path.display()))?;
    parse_theme_file_json(&content)
}

pub fn export_echo_theme_json(palette: &ThemeFilePalette) -> Result<String, String> {
    let value = json!({
        "$schema": ECHO_THEME_SCHEMA_URL,
        "metadata": {
            "name": palette.name.as_deref().unwrap_or("Echo Theme")
        },
        "colors": {
            "accent": palette.accent.as_deref().unwrap_or("#7f86ff"),
            "bg_void": palette.bg_void,
            "bg_surface": palette.bg_surface,
            "bg_plate": palette.bg_plate,
            "bg_elevated": palette.bg_elevated,
            "bg_hover": palette.bg_hover,
            "text_primary": palette.text_primary,
            "text_secondary": palette.text_secondary,
            "text_muted": palette.text_muted,
            "border": palette.border,
            "border_strong": palette.border_strong
        }
    });

    let exported =
        serde_json::to_string_pretty(&value).map_err(|e| format!("Failed to export theme: {e}"))?;
    parse_theme_file_json(&exported)?;
    Ok(exported)
}

fn parse_theme_value(value: &Value) -> Result<ThemeFilePalette, String> {
    let colors = value
        .get("colors")
        .ok_or_else(|| "Theme file must contain a 'colors' object".to_string())?;

    if colors.get("bg_void").is_some() || colors.get("bgSurface").is_some() {
        return parse_echo_palette(value, colors);
    }

    parse_nested_palette(value, colors)
}

fn parse_echo_palette(root: &Value, colors: &Value) -> Result<ThemeFilePalette, String> {
    Ok(ThemeFilePalette {
        name: string_at(root, &["metadata", "name"]).or_else(|| string_at(root, &["name"])),
        accent: optional_color_at(colors, &["accent"])?,
        bg_void: required_color_at(colors, &["bg_void"])
            .or_else(|_| required_color_at(colors, &["bgVoid"]))?,
        bg_surface: required_color_at(colors, &["bg_surface"])
            .or_else(|_| required_color_at(colors, &["bgSurface"]))?,
        bg_plate: required_color_at(colors, &["bg_plate"])
            .or_else(|_| required_color_at(colors, &["bgPlate"]))?,
        bg_elevated: required_color_at(colors, &["bg_elevated"])
            .or_else(|_| required_color_at(colors, &["bgElevated"]))?,
        bg_hover: required_color_at(colors, &["bg_hover"])
            .or_else(|_| required_color_at(colors, &["bgHover"]))?,
        text_primary: required_color_at(colors, &["text_primary"])
            .or_else(|_| required_color_at(colors, &["textPrimary"]))?,
        text_secondary: required_color_at(colors, &["text_secondary"])
            .or_else(|_| required_color_at(colors, &["textSecondary"]))?,
        text_muted: required_color_at(colors, &["text_muted"])
            .or_else(|_| required_color_at(colors, &["textMuted"]))?,
        border: required_color_at(colors, &["border"])?,
        border_strong: required_color_at(colors, &["border_strong"])
            .or_else(|_| required_color_at(colors, &["borderStrong"]))?,
    })
}

fn parse_nested_palette(root: &Value, colors: &Value) -> Result<ThemeFilePalette, String> {
    Ok(ThemeFilePalette {
        name: string_at(root, &["metadata", "name"]).or_else(|| string_at(root, &["name"])),
        accent: optional_color_at(colors, &["accent"])?,
        bg_void: first_color(
            colors,
            &[
                &["window", "background"],
                &["splits", "background"],
                &["messages", "backgrounds", "regular"],
            ],
        )?,
        bg_surface: first_color(
            colors,
            &[
                &["messages", "backgrounds", "regular"],
                &["splits", "background"],
                &["window", "background"],
            ],
        )?,
        bg_plate: first_color(
            colors,
            &[
                &["splits", "header", "background"],
                &["messages", "backgrounds", "alternate"],
                &["tabs", "regular", "backgrounds", "regular"],
            ],
        )?,
        bg_elevated: first_color(
            colors,
            &[
                &["overlayMessages", "background"],
                &["messages", "backgrounds", "alternate"],
                &["window", "background"],
            ],
        )?,
        bg_hover: first_color(
            colors,
            &[
                &["messages", "selection"],
                &["tabs", "regular", "backgrounds", "hover"],
                &["messages", "backgrounds", "alternate"],
            ],
        )?,
        text_primary: first_color(
            colors,
            &[
                &["messages", "textColors", "regular"],
                &["window", "text"],
                &["splits", "header", "text"],
            ],
        )?,
        text_secondary: first_color(
            colors,
            &[
                &["messages", "textColors", "system"],
                &["splits", "header", "text"],
                &["window", "text"],
            ],
        )?,
        text_muted: first_color(
            colors,
            &[
                &["messages", "textColors", "chatPlaceholder"],
                &["overlayMessages", "textColors", "chatPlaceholder"],
                &["messages", "textColors", "system"],
            ],
        )?,
        border: first_color(
            colors,
            &[
                &["splits", "header", "border"],
                &["tabs", "dividerLine"],
                &["splits", "messageSeperator"],
            ],
        )?,
        border_strong: first_color(
            colors,
            &[
                &["splits", "header", "focusedBorder"],
                &["splits", "header", "border"],
                &["tabs", "selected", "line", "regular"],
            ],
        )?,
    })
}

fn first_color(value: &Value, paths: &[&[&str]]) -> Result<String, String> {
    for path in paths {
        if let Some(color) = optional_color_at(value, path)? {
            return Ok(color);
        }
    }
    Err(format!(
        "Theme file is missing color path {}",
        paths[0].join(".")
    ))
}

fn required_color_at(value: &Value, path: &[&str]) -> Result<String, String> {
    optional_color_at(value, path)?
        .ok_or_else(|| format!("Theme file is missing color path {}", path.join(".")))
}

fn optional_color_at(value: &Value, path: &[&str]) -> Result<Option<String>, String> {
    let Some(raw) = string_at(value, path) else {
        return Ok(None);
    };
    normalize_qt_color(&raw)
        .map(Some)
        .ok_or_else(|| format!("Unsupported color '{}' at {}", raw, path.join(".")))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(|value| value.to_string())
}

fn normalize_qt_color(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value == "transparent" {
        return Some("#000000".to_string());
    }

    if let Some(hex) = value.strip_prefix('#') {
        if !hex.is_ascii() {
            return None;
        }
        return match hex.len() {
            3 => {
                let r = &hex[0..1];
                let g = &hex[1..2];
                let b = &hex[2..3];
                Some(format!("#{r}{r}{g}{g}{b}{b}"))
            }
            6 => u32::from_str_radix(hex, 16).ok().map(|_| format!("#{hex}")),
            8 => u32::from_str_radix(hex, 16)
                .ok()
                .map(|_| format!("#{}", &hex[2..])),
            _ => None,
        };
    }

    named_color(&value).map(str::to_string)
}

fn named_color(name: &str) -> Option<&'static str> {
    match name {
        "black" => Some("#000000"),
        "white" => Some("#ffffff"),
        "red" => Some("#ff0000"),
        "green" => Some("#008000"),
        "blue" => Some("#0000ff"),
        "cyan" | "aqua" => Some("#00ffff"),
        "magenta" | "fuchsia" => Some("#ff00ff"),
        "yellow" => Some("#ffff00"),
        "gray" | "grey" => Some("#808080"),
        "darkgray" | "darkgrey" => Some("#a9a9a9"),
        "lightgray" | "lightgrey" => Some("#d3d3d3"),
        "orange" => Some("#ffa500"),
        "purple" => Some("#800080"),
        "pink" => Some("#ffc0cb"),
        "teal" => Some("#008080"),
        "navy" => Some("#000080"),
        "silver" => Some("#c0c0c0"),
        "maroon" => Some("#800000"),
        "lime" => Some("#00ff00"),
        "olive" => Some("#808000"),
        _ => None,
    }
}
