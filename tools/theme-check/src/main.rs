#![allow(dead_code)]

#[path = "../../../src/backend/theme_files.rs"]
mod theme_files;

use std::hint::black_box;
use std::time::Instant;
use theme_files::{ThemeFilePalette, export_echo_theme_json, parse_theme_file_json};

const ITERS: usize = 100_000;

fn main() {
    let nested = sample_nested_theme();
    let nested_parsed = parse_theme_file_json(nested).expect("Nested theme should parse");
    assert_eq!(nested_parsed.accent.as_deref(), Some("#7f86ff"));
    assert_eq!(nested_parsed.bg_void, "#050505");
    assert_eq!(nested_parsed.bg_surface, "#101010");
    assert_eq!(nested_parsed.text_primary, "#eeeeee");
    assert_eq!(nested_parsed.border_strong, "#7f86ff");

    let nested_fallback = sample_nested_fallback_theme();
    let fallback_parsed =
        parse_theme_file_json(nested_fallback).expect("Fallback nested theme should parse");
    assert_eq!(fallback_parsed.accent, None);
    assert_eq!(fallback_parsed.bg_plate, "#222222");
    assert_eq!(fallback_parsed.bg_hover, "#112233");
    assert_eq!(fallback_parsed.text_primary, "#ffffff");
    assert_eq!(fallback_parsed.text_muted, "#808080");
    assert_eq!(fallback_parsed.border, "#444444");
    assert_eq!(fallback_parsed.border_strong, "#335577");

    let echo = sample_echo_theme();
    let echo_parsed = parse_theme_file_json(echo).expect("Echo flat theme should parse");
    assert_eq!(echo_parsed.name.as_deref(), Some("Echo Flat"));
    assert_eq!(echo_parsed.bg_hover, "#202020");
    assert_eq!(echo_parsed.text_muted, "#777777");

    let exported = export_echo_theme_json(&echo_parsed).expect("Echo flat theme should export");
    let round_trip = parse_theme_file_json(&exported).expect("Exported theme should parse");
    assert_eq!(round_trip, echo_parsed);
    assert!(exported.contains(r#""$schema": "https://echo.local/EchoTheme.schema.json""#));

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(
        &nested_parsed,
        &fallback_parsed,
        &echo_parsed,
        &exported,
        &round_trip,
    );

    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..ITERS {
        let parsed = parse_theme_file_json(black_box(nested)).unwrap();
        checksum += parsed.bg_surface.len() + parsed.text_primary.len();
    }
    print_measurement("parse", "nested_theme", ITERS, start, checksum);

    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..ITERS {
        let parsed = parse_theme_file_json(black_box(nested_fallback)).unwrap();
        checksum += parsed.bg_plate.len() + parsed.border_strong.len();
    }
    print_measurement("parse", "nested_fallback_theme", ITERS, start, checksum);

    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..ITERS {
        let parsed = parse_theme_file_json(black_box(echo)).unwrap();
        checksum += parsed.bg_surface.len() + parsed.text_primary.len();
    }
    print_measurement("parse", "echo_flat_theme", ITERS, start, checksum);

    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..ITERS {
        let exported = export_echo_theme_json(black_box(&echo_parsed)).unwrap();
        checksum += exported.len();
    }
    print_measurement("export", "echo_flat_theme", ITERS, start, checksum);
}

fn print_smoke_rows(
    nested: &ThemeFilePalette,
    fallback: &ThemeFilePalette,
    echo: &ThemeFilePalette,
    exported: &str,
    round_trip: &ThemeFilePalette,
) {
    print_fixture(
        "theme_nested_argb_accent",
        nested.accent.as_deref().unwrap_or("none"),
        "#7f86ff",
    );
    print_fixture("theme_nested_bg_void", &nested.bg_void, "#050505");
    print_fixture("theme_nested_bg_surface", &nested.bg_surface, "#101010");
    print_fixture("theme_nested_text_primary", &nested.text_primary, "#eeeeee");
    print_fixture(
        "theme_nested_border_strong",
        &nested.border_strong,
        "#7f86ff",
    );
    print_fixture(
        "theme_nested_fallback_no_accent",
        bool_text(fallback.accent.is_none()),
        "true",
    );
    print_fixture(
        "theme_nested_fallback_bg_plate",
        &fallback.bg_plate,
        "#222222",
    );
    print_fixture(
        "theme_nested_fallback_bg_hover",
        &fallback.bg_hover,
        "#112233",
    );
    print_fixture(
        "theme_nested_fallback_named_text",
        &fallback.text_primary,
        "#ffffff",
    );
    print_fixture(
        "theme_nested_fallback_muted_text",
        &fallback.text_muted,
        "#808080",
    );
    print_fixture("theme_nested_fallback_border", &fallback.border, "#444444");
    print_fixture(
        "theme_nested_fallback_border_strong",
        &fallback.border_strong,
        "#335577",
    );
    print_fixture(
        "theme_echo_name",
        echo.name.as_deref().unwrap_or("none"),
        "Echo Flat",
    );
    print_fixture("theme_echo_bg_hover", &echo.bg_hover, "#202020");
    print_fixture("theme_echo_text_muted", &echo.text_muted, "#777777");
    print_fixture(
        "theme_export_schema_present",
        bool_text(exported.contains(r#""$schema": "https://echo.local/EchoTheme.schema.json""#)),
        "true",
    );
    print_fixture(
        "theme_export_round_trip",
        bool_text(round_trip == echo),
        "true",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn print_measurement(kind: &str, name: &str, units: usize, start: Instant, checksum: usize) {
    let elapsed = start.elapsed();
    let nanos_per_unit = elapsed.as_nanos() as f64 / units as f64;
    let units_per_second = units as f64 / elapsed.as_secs_f64();
    println!("{kind},{name},{units},{nanos_per_unit:.2},{units_per_second:.0},{checksum}");
}

fn sample_echo_theme() -> &'static str {
    r##"{
        "$schema": "https://echo.local/EchoTheme.schema.json",
        "name": "Echo Flat",
        "colors": {
            "accent": "#7f86ff",
            "bg_void": "#050505",
            "bg_surface": "#101010",
            "bg_plate": "#080808",
            "bg_elevated": "#141414",
            "bg_hover": "#202020",
            "text_primary": "#eeeeee",
            "text_secondary": "#aaaaaa",
            "text_muted": "#777777",
            "border": "#303030",
            "border_strong": "#505050"
        }
    }"##
}

fn sample_nested_theme() -> &'static str {
    r##"{
        "metadata": {
            "iconTheme": "light",
            "fallbackTheme": "Dark"
        },
        "colors": {
            "accent": "#ff7f86ff",
            "messages": {
                "backgrounds": {
                    "regular": "#101010",
                    "alternate": "#181818"
                },
                "disabled": "#333333",
                "highlightAnimationEnd": "#202020",
                "highlightAnimationStart": "#303030",
                "selection": "#202020",
                "textColors": {
                    "caret": "#ffffff",
                    "chatPlaceholder": "#777777",
                    "link": "#7f86ff",
                    "regular": "#eeeeee",
                    "system": "#aaaaaa"
                }
            },
            "overlayMessages": {
                "backgrounds": {
                    "regular": "#101010",
                    "alternate": "#181818"
                },
                "disabled": "#333333",
                "selection": "#202020",
                "background": "#141414",
                "textColors": {
                    "caret": "#ffffff",
                    "chatPlaceholder": "#777777",
                    "link": "#7f86ff",
                    "regular": "#eeeeee",
                    "system": "#aaaaaa"
                }
            },
            "scrollbars": {
                "background": "#050505",
                "thumb": "#303030",
                "thumbSelected": "#505050"
            },
            "splits": {
                "background": "#050505",
                "dropPreview": "#202020",
                "dropPreviewBorder": "#303030",
                "dropTargetRect": "#202020",
                "dropTargetRectBorder": "#303030",
                "header": {
                    "background": "#080808",
                    "border": "#303030",
                    "focusedBackground": "#101010",
                    "focusedBorder": "#7f86ff",
                    "focusedText": "#eeeeee",
                    "text": "#aaaaaa"
                },
                "input": {
                    "background": "#101010",
                    "backgroundPulse": "#202020",
                    "text": "#eeeeee"
                },
                "messageSeperator": "#303030",
                "resizeHandle": "#303030",
                "resizeHandleBackground": "#050505"
            },
            "tabs": {
                "liveIndicator": "#ff0000",
                "rerunIndicator": "#ffa500",
                "dividerLine": "#303030",
                "highlighted": {
                    "backgrounds": {
                        "hover": "#202020",
                        "regular": "#101010",
                        "unfocused": "#080808"
                    },
                    "line": {
                        "hover": "#7f86ff",
                        "regular": "#7f86ff",
                        "unfocused": "#303030"
                    },
                    "text": "#eeeeee"
                },
                "newMessage": {
                    "backgrounds": {
                        "hover": "#202020",
                        "regular": "#101010",
                        "unfocused": "#080808"
                    },
                    "line": {
                        "hover": "#7f86ff",
                        "regular": "#7f86ff",
                        "unfocused": "#303030"
                    },
                    "text": "#eeeeee"
                },
                "regular": {
                    "backgrounds": {
                        "hover": "#202020",
                        "regular": "#101010",
                        "unfocused": "#080808"
                    },
                    "line": {
                        "hover": "#7f86ff",
                        "regular": "#303030",
                        "unfocused": "#303030"
                    },
                    "text": "#eeeeee"
                },
                "selected": {
                    "backgrounds": {
                        "hover": "#202020",
                        "regular": "#101010",
                        "unfocused": "#080808"
                    },
                    "line": {
                        "hover": "#7f86ff",
                        "regular": "#7f86ff",
                        "unfocused": "#303030"
                    },
                    "text": "#eeeeee"
                }
            },
            "window": {
                "background": "#050505",
                "text": "#eeeeee"
            }
        }
    }"##
}

fn sample_nested_fallback_theme() -> &'static str {
    r##"{
        "metadata": {
            "fallbackTheme": "Dark"
        },
        "colors": {
            "window": {
                "background": "#010203",
                "text": "white"
            },
            "messages": {
                "backgrounds": {
                    "regular": "#111",
                    "alternate": "#222222"
                },
                "textColors": {
                    "regular": "white",
                    "system": "gray"
                }
            },
            "splits": {
                "messageSeperator": "#444444"
            },
            "tabs": {
                "dividerLine": "#444444",
                "regular": {
                    "backgrounds": {
                        "hover": "#123"
                    }
                },
                "selected": {
                    "line": {
                        "regular": "#335577"
                    }
                }
            }
        }
    }"##
}
