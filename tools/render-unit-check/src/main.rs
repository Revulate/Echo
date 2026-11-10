#![allow(dead_code, unused_variables)]

use std::sync::Arc;

#[path = "../../../src/backend/ascii_art.rs"]
mod ascii_art;

mod backend {
    pub mod emotes {
        use std::sync::Arc;

        #[derive(Clone, Debug)]
        pub struct Emote {
            pub id: Arc<str>,
            pub name: Arc<str>,
            pub zero_width: bool,
        }
    }
}

mod slots;

use backend::emotes::Emote;
use slots::render_units::{SlotRenderUnit, group_resolved_slots};
use slots::resolver::ResolvedSlot;

fn main() {
    println!("kind,name,value,checksum");

    print_ascii_art_fixtures();

    let grouped = group_resolved_slots(
        [
            ResolvedSlot::Emote(emote("base", false)),
            ResolvedSlot::Emote(emote("overlay-a", true)),
            ResolvedSlot::Emote(emote("overlay-b", true)),
        ],
        true,
    );
    print_fixture(
        "zero_width_enabled_unit_count",
        grouped.len().to_string(),
        "1",
    );
    print_fixture(
        "zero_width_enabled_stack",
        emote_stack_summary(&grouped),
        "base+[overlay-a,overlay-b]",
    );
    print_fixture(
        "zero_width_for_each_order",
        visited_emote_summary(&grouped),
        "base|overlay-a|overlay-b",
    );

    let disabled = group_resolved_slots(
        [
            ResolvedSlot::Emote(emote("base", false)),
            ResolvedSlot::Emote(emote("overlay-a", true)),
        ],
        false,
    );
    print_fixture(
        "zero_width_disabled_unit_count",
        disabled.len().to_string(),
        "2",
    );
    print_fixture(
        "zero_width_disabled_units",
        emote_stack_summary(&disabled),
        "base|overlay-a",
    );

    let leading_overlay = group_resolved_slots(
        [
            ResolvedSlot::Emote(emote("leading-overlay", true)),
            ResolvedSlot::Emote(emote("base", false)),
        ],
        true,
    );
    print_fixture(
        "zero_width_leading_overlay",
        emote_stack_summary(&leading_overlay),
        "leading-overlay|base",
    );

    let separated = group_resolved_slots(
        [
            ResolvedSlot::Emote(emote("base", false)),
            ResolvedSlot::Text(Arc::from("gap")),
            ResolvedSlot::Emote(emote("overlay-after-text", true)),
        ],
        true,
    );
    print_fixture(
        "zero_width_text_breaks_stack",
        unit_kind_summary(&separated),
        "emote|text|emote",
    );
    print_fixture(
        "zero_width_text_breaks_names",
        emote_stack_summary(&separated),
        "base|overlay-after-text",
    );

    let mixed = group_resolved_slots(
        [
            ResolvedSlot::Url {
                url: Arc::from("https://example.com"),
                embed_id: 42,
            },
            ResolvedSlot::Mention(Arc::from("forsen")),
            ResolvedSlot::Emoji {
                emoji: Arc::from("😀"),
                url: "emoji://grinning".to_string(),
            },
            ResolvedSlot::TwitchGif {
                id: Arc::from("fixture"),
                url: Arc::from("https://media.giphy.com/media/fixture/giphy.gif?keep=query"),
                alt_text: Arc::from("[caption]"),
            },
        ],
        true,
    );
    print_fixture(
        "render_unit_kind_mix",
        unit_kind_summary(&mixed),
        "url|mention|emoji|gif",
    );
    print_fixture(
        "render_unit_substantive_count",
        mixed
            .iter()
            .filter(|unit| unit.is_substantive_content())
            .count()
            .to_string(),
        "4",
    );
    print_fixture(
        "render_unit_url_visit",
        visited_url_summary(&mixed),
        "https://example.com#42",
    );
}

fn print_ascii_art_fixtures() {
    let ordinary = ascii_art::classify("ordinary chat with one ░ decoration");
    print_fixture("ascii_short_decoration", art_kind_name(ordinary), "none");

    let multiline = ascii_art::classify("========\n========\n========");
    print_fixture(
        "ascii_multiline_repetition",
        art_kind_name(multiline),
        "collapsible-multiline",
    );

    let below_wrapped_threshold = ascii_art::classify(&"⣿".repeat(47));
    print_fixture(
        "ascii_wrapped_below_threshold",
        art_kind_name(below_wrapped_threshold),
        "none",
    );

    let twitch_wrapped = ascii_art::classify(&"⣿".repeat(48));
    print_fixture(
        "ascii_twitch_wrapped_braille",
        art_kind_name(twitch_wrapped),
        "twitch-wrapped",
    );
    print_fixture(
        "ascii_twitch_wrapped_punctuation",
        art_kind_name(ascii_art::classify(&"/\\".repeat(24))),
        "twitch-wrapped",
    );
    print_fixture(
        "ascii_wide_split_reference_width",
        twitch_wrapped.layout_width(720.0).to_string(),
        "340",
    );
    print_fixture(
        "ascii_narrow_split_real_width",
        twitch_wrapped.layout_width(280.0).to_string(),
        "280",
    );
    print_fixture(
        "ordinary_wide_split_real_width",
        ordinary.layout_width(720.0).to_string(),
        "720",
    );
}

fn art_kind_name(kind: ascii_art::AsciiArtKind) -> &'static str {
    match kind {
        ascii_art::AsciiArtKind::None => "none",
        ascii_art::AsciiArtKind::CollapsibleMultiline => "collapsible-multiline",
        ascii_art::AsciiArtKind::TwitchWrapped => "twitch-wrapped",
    }
}

fn emote(name: &str, zero_width: bool) -> Arc<Emote> {
    Arc::new(Emote {
        id: Arc::from(name),
        name: Arc::from(name),
        zero_width,
    })
}

fn emote_stack_summary(units: &[SlotRenderUnit]) -> String {
    units
        .iter()
        .filter_map(|unit| match unit {
            SlotRenderUnit::Emote(group) if group.overlays.is_empty() => {
                Some(group.base.name.to_string())
            }
            SlotRenderUnit::Emote(group) => Some(format!(
                "{}+[{}]",
                group.base.name,
                group
                    .overlays
                    .iter()
                    .map(|overlay| overlay.name.as_ref())
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn visited_emote_summary(units: &[SlotRenderUnit]) -> String {
    let mut names = Vec::new();
    for unit in units {
        unit.for_each_emote(|emote| names.push(emote.name.to_string()));
    }
    names.join("|")
}

fn unit_kind_summary(units: &[SlotRenderUnit]) -> String {
    units
        .iter()
        .map(|unit| match unit {
            SlotRenderUnit::Text(_) => "text",
            SlotRenderUnit::Emote(_) => "emote",
            SlotRenderUnit::Url { .. } => "url",
            SlotRenderUnit::Mention(_) => "mention",
            SlotRenderUnit::Emoji { .. } => "emoji",
            SlotRenderUnit::TwitchGif { .. } => "gif",
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn visited_url_summary(units: &[SlotRenderUnit]) -> String {
    let mut urls = Vec::new();
    for unit in units {
        unit.for_each_url(|url, embed_id| urls.push(format!("{url}#{embed_id}")));
    }
    urls.join("|")
}

fn print_fixture(name: &str, actual: impl AsRef<str>, expected: &str) {
    let actual = actual.as_ref();
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
