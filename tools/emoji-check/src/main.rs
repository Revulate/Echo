#![allow(dead_code)]

use echo_core::{
    slot::MessageSlot,
    slot_parser::{NoEmotes, TwitchEmoteMap, parse_to_slots},
};
use echo_text_core::{EmojiStyle, TextSegment, emoji::emoji_prefix, get_emoji_url, parse_emoji};

fn main() {
    println!("kind,name,value,checksum");

    let basic = parse_emoji("Hello 👋 World", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_basic",
        &segment_signature(&basic),
        "text:6|emoji:👋|text:6",
    );

    let family = parse_emoji("Family: 👨‍👩‍👧", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_zwj_family",
        &segment_signature(&family),
        "text:8|emoji:👨‍👩‍👧",
    );

    let inline_family = parse_emoji("Family: 👨‍👩‍👧 end", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_inline_zwj_family",
        &segment_signature(&inline_family),
        "text:8|emoji:👨‍👩‍👧|text:4",
    );

    let flag = parse_emoji("US: 🇺🇸", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_flag",
        &segment_signature(&flag),
        "text:4|emoji:🇺🇸",
    );

    let multiple = parse_emoji("👋🎉🚀", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_multiple",
        &segment_signature(&multiple),
        "emoji:👋|emoji:🎉|emoji:🚀",
    );

    let no_emoji = parse_emoji("Hello World", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_no_emoji",
        &segment_signature(&no_emoji),
        "text:11",
    );

    let empty = parse_emoji("", &EmojiStyle::Twemoji);
    print_fixture("emoji_parse_empty", &segment_signature(&empty), "text:0");

    let ordinary_symbol = parse_emoji("✧", &EmojiStyle::Twemoji);
    print_fixture(
        "unicode_symbol_preserved_as_text",
        &matches!(&ordinary_symbol[..], [TextSegment::Text(text)] if text == "✧").to_string(),
        "true",
    );

    let keycap = parse_emoji("1️⃣", &EmojiStyle::Twemoji);
    print_fixture(
        "emoji_parse_keycap",
        &segment_signature(&keycap),
        "emoji:1️⃣",
    );

    // `emojis::get` requires a complete sequence, so the old slot parser
    // passed this entire suffix and skipped its inline emoji branch.
    print_fixture(
        "legacy_suffix_lookup_misses_inline_emoji",
        &emojis::get("😀tail").is_none().to_string(),
        "true",
    );

    let inline_slots = parse_to_slots("left😀right", &TwitchEmoteMap::default(), &NoEmotes);
    print_fixture(
        "slot_parser_inline_emoji",
        &slot_signature(&inline_slots),
        "text:left|emoji:😀|text:right",
    );

    let sequences = parse_to_slots("👨‍👩‍👧🇺🇸1️⃣", &TwitchEmoteMap::default(), &NoEmotes);
    print_fixture(
        "slot_parser_bounded_sequences",
        &slot_signature(&sequences),
        "emoji:👨‍👩‍👧|emoji:🇺🇸|emoji:1️⃣",
    );

    let mut twitch_emotes = TwitchEmoteMap::default();
    twitch_emotes.insert(1, ("25".to_string(), 6));
    let twitch_offset_slots = parse_to_slots("😀Kappa", &twitch_emotes, &NoEmotes);
    print_fixture(
        "slot_parser_emoji_preserves_twitch_character_offsets",
        &slot_signature(&twitch_offset_slots),
        "emoji:😀|twitch:Kappa",
    );

    let precedence_slots = parse_to_slots(
        "https://example.com @user 😀tail",
        &TwitchEmoteMap::default(),
        &NoEmotes,
    );
    print_fixture(
        "slot_parser_url_and_mention_precede_later_emoji",
        &slot_signature(&precedence_slots),
        "url:https://example.com|mention:user|emoji:😀|text:tail",
    );

    let source_spelling = emoji_prefix("♥tail")
        .map(|(text, _)| text)
        .unwrap_or_default();
    print_fixture(
        "emoji_prefix_preserves_source_spelling",
        source_spelling,
        "♥",
    );

    print_fixture(
        "emoji_url_twemoji",
        &get_emoji_url("👋", &EmojiStyle::Twemoji),
        "https://cdnjs.cloudflare.com/ajax/libs/twemoji/14.0.2/72x72/1f44b.png",
    );
    print_fixture(
        "emoji_url_noto",
        &get_emoji_url("😀", &EmojiStyle::Noto),
        "https://cdn.jsdelivr.net/npm/emoji-datasource-google@16.0.1/img/google/64/1f600.png",
    );
    print_fixture(
        "emoji_url_native_fallback",
        &get_emoji_url("💤", &EmojiStyle::Native),
        "https://cdnjs.cloudflare.com/ajax/libs/twemoji/14.0.2/72x72/1f4a4.png",
    );
    print_fixture(
        "emoji_url_variation_selector",
        &get_emoji_url("✈️", &EmojiStyle::Twemoji),
        "https://cdnjs.cloudflare.com/ajax/libs/twemoji/14.0.2/72x72/2708.png",
    );

    let groups: Vec<_> = emojis::Group::iter().collect();
    print_fixture("emoji_group_count", &groups.len().to_string(), "9");
    print_fixture(
        "emoji_group_first",
        &format!("{:?}", groups[0]),
        "SmileysAndEmotion",
    );

    let group_order = groups
        .iter()
        .map(|group| format!("{group:?}"))
        .collect::<Vec<_>>()
        .join("|");
    print_fixture(
        "emoji_group_order",
        &group_order,
        "SmileysAndEmotion|PeopleAndBody|AnimalsAndNature|FoodAndDrink|TravelAndPlaces|Activities|Objects|Symbols|Flags",
    );

    let total: usize = groups.iter().map(|group| group.emojis().count()).sum();
    print_threshold("emoji_total_count_gt_1500", total, 1500);
}

fn segment_signature(segments: &[TextSegment]) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            TextSegment::Text(text) => format!("text:{}", text.chars().count()),
            TextSegment::Emoji { char, .. } => format!("emoji:{char}"),
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn slot_signature(slots: &[MessageSlot]) -> String {
    slots
        .iter()
        .map(|slot| match slot {
            MessageSlot::Text(text) => format!("text:{text}"),
            MessageSlot::Emoji { emoji } => format!("emoji:{emoji}"),
            MessageSlot::TwitchGif(gif) => format!("gif:{}", gif.alt_text),
            MessageSlot::TwitchEmote { name, .. } => format!("twitch:{name}"),
            MessageSlot::Emote(emote) => format!("emote:{}", emote.name),
            MessageSlot::Url { url, .. } => format!("url:{url}"),
            MessageSlot::Mention { username } => format!("mention:{username}"),
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn print_threshold(name: &str, actual: usize, minimum: usize) {
    assert!(
        actual > minimum,
        "threshold mismatch for {name}: expected > {minimum}, got {actual}"
    );
    println!("fixture,{name},{actual},{actual}");
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
