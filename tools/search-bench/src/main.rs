//! Benchmark: chat-search query compile and match cost in `echo_core::search`
//! over 5,000 synthetic messages, after a smoke check of each query's match
//! count. `scripts/bench/run-pgo-build.py` parses the CSV, including `checksum`.

use echo_core::search::{SearchMessage, compile_search_query};
use std::hint::black_box;
use std::time::{Duration, Instant};

const MESSAGE_COUNT: usize = 5_000;
const COMPILE_ITERS: usize = 20_000;
const MATCH_ITERS: usize = 250;

const TYPE_REPLY: u16 = 1 << 0;
const TYPE_FIRST: u16 = 1 << 1;
const TYPE_HIGHLIGHTED: u16 = 1 << 2;
const TYPE_DELETED: u16 = 1 << 3;
const TYPE_SUBSCRIPTION: u16 = 1 << 4;
const TYPE_MODERATION: u16 = 1 << 5;
const TYPE_SYSTEM: u16 = 1 << 6;
const TYPE_CHEER: u16 = 1 << 7;
const TYPE_REDEMPTION: u16 = 1 << 8;

#[derive(Clone)]
struct BenchMessage {
    channel: &'static str,
    sender: &'static str,
    display_name: &'static str,
    content: String,
    badges: Vec<&'static str>,
    has_link: bool,
    types: u16,
}

impl SearchMessage for BenchMessage {
    fn content(&self) -> &str {
        &self.content
    }

    fn sender(&self) -> &str {
        self.sender
    }

    fn display_name(&self) -> &str {
        self.display_name
    }

    fn channel(&self) -> &str {
        self.channel
    }

    fn has_link(&self) -> bool {
        self.has_link
    }

    fn has_badge(&self, badge: &str) -> bool {
        self.badges.iter().any(|value| {
            value.eq_ignore_ascii_case(badge)
                || value
                    .split_once(':')
                    .map(|(name, _)| name.eq_ignore_ascii_case(badge))
                    .unwrap_or(false)
        })
    }

    fn has_subtier(&self, tier: &str) -> bool {
        self.badges.iter().any(|value| {
            let Some((name, stored_tier)) = value.split_once(':') else {
                return false;
            };
            name.eq_ignore_ascii_case("subscriber")
                && (stored_tier.eq_ignore_ascii_case(tier)
                    || match tier {
                        "1" => stored_tier.starts_with('1'),
                        "2" => stored_tier.starts_with('2'),
                        "3" => stored_tier.starts_with('3'),
                        _ => false,
                    })
        })
    }

    fn has_type(&self, value: &str) -> bool {
        match value {
            "reply" => self.types & TYPE_REPLY != 0,
            "first-msg" | "first-message" => self.types & TYPE_FIRST != 0,
            "highlighted" => self.types & TYPE_HIGHLIGHTED != 0,
            "deleted" | "disabled" => self.types & TYPE_DELETED != 0,
            "sub" | "subscription" => self.types & TYPE_SUBSCRIPTION != 0,
            "timeout" | "ban" => self.types & TYPE_MODERATION != 0,
            "system" => self.types & TYPE_SYSTEM != 0,
            "cheer-msg" => self.types & TYPE_CHEER != 0,
            "redemption" => self.types & TYPE_REDEMPTION != 0,
            _ => false,
        }
    }
}

struct Measurement {
    name: &'static str,
    elapsed: Duration,
    units: usize,
    checksum: usize,
}

fn make_message(id: usize) -> BenchMessage {
    let has_link = id.is_multiple_of(5);
    let content = if has_link {
        format!("clip merge candidate https://example.com/{id}")
    } else if id.is_multiple_of(11) {
        format!("reply thread update {id}")
    } else {
        format!("normal chat line {id}")
    };

    let mut types = 0;
    if id.is_multiple_of(11) {
        types |= TYPE_REPLY;
    }
    if id.is_multiple_of(53) {
        types |= TYPE_FIRST;
    }
    if id.is_multiple_of(97) {
        types |= TYPE_HIGHLIGHTED;
    }
    if id.is_multiple_of(101) {
        types |= TYPE_DELETED;
    }
    if id.is_multiple_of(19) {
        types |= TYPE_SUBSCRIPTION;
    }
    if id.is_multiple_of(29) {
        types |= TYPE_MODERATION;
    }
    if id.is_multiple_of(31) {
        types |= TYPE_SYSTEM;
    }
    if id.is_multiple_of(23) {
        types |= TYPE_CHEER;
    }
    if id.is_multiple_of(37) {
        types |= TYPE_REDEMPTION;
    }

    let mut badges = if id.is_multiple_of(17) {
        vec!["moderator:1", "subscriber:3"]
    } else {
        vec!["subscriber:1"]
    };
    if id.is_multiple_of(41) {
        badges.push("no_video:1");
    }

    BenchMessage {
        channel: if id.is_multiple_of(7) {
            "supinic"
        } else {
            "alice"
        },
        sender: if id.is_multiple_of(13) {
            "pajbot"
        } else {
            "bob"
        },
        display_name: if id.is_multiple_of(13) {
            "pajbot"
        } else {
            "Bob"
        },
        content,
        badges,
        has_link,
        types,
    }
}

fn measure_compile(query: &'static str) -> Measurement {
    let start = Instant::now();
    let mut checksum = 0;
    for _ in 0..COMPILE_ITERS {
        let compiled = compile_search_query(black_box(query), false).unwrap();
        checksum += compiled.is_empty() as usize;
        black_box(compiled);
    }
    Measurement {
        name: query,
        elapsed: start.elapsed(),
        units: COMPILE_ITERS,
        checksum,
    }
}

fn measure_match(name: &'static str, query: &str, messages: &[BenchMessage]) -> Measurement {
    let compiled = compile_search_query(query, false).unwrap();
    let start = Instant::now();
    let mut checksum = 0;
    for _ in 0..MATCH_ITERS {
        checksum += messages
            .iter()
            .filter(|msg| compiled.matches(black_box(*msg)))
            .count();
    }
    Measurement {
        name,
        elapsed: start.elapsed(),
        units: MATCH_ITERS * messages.len(),
        checksum,
    }
}

fn print_measurement(kind: &str, measurement: &Measurement) {
    let nanos_per_unit = measurement.elapsed.as_nanos() as f64 / measurement.units as f64;
    let units_per_second = measurement.units as f64 / measurement.elapsed.as_secs_f64();
    let name = measurement.name.replace(',', ";");
    println!(
        "{kind},{},{},{:.2},{:.0},{}",
        name, measurement.units, nanos_per_unit, units_per_second, measurement.checksum
    );
}

fn main() {
    let messages: Vec<_> = (0..MESSAGE_COUNT).map(make_message).collect();
    smoke_check_queries(&messages);

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    for query in [
        "merge",
        "has:link from:bob merge",
        "in:alice,supinic !from:carol is:reply",
        r#"regex:"clip merge candidate https://example\.com/\d+""#,
        "badge:moderator subtier:3",
    ] {
        print_measurement("compile", &measure_compile(query));
    }

    for (name, query) in [
        ("match_5k_term", "merge"),
        ("match_5k_has_link_from_term", "has:link from:bob merge"),
        (
            "match_5k_channel_negated_reply",
            "in:alice,supinic !from:carol is:reply",
        ),
        (
            "match_5k_regex",
            r#"regex:"clip merge candidate https://example\.com/\d+""#,
        ),
        ("match_5k_badge_subtier", "badge:moderator subtier:3"),
        (
            "match_5k_wiki_is_flags",
            "is:deleted,sub,timeout,highlighted,system,first-msg,cheer-msg,redemption,reply",
        ),
    ] {
        print_measurement("match", &measure_match(name, query, &messages));
    }
}

fn smoke_check_queries(messages: &[BenchMessage]) {
    for (name, query, expected) in [
        ("term", "merge", 1_000),
        ("has-link-from-term", "has:link from:bob merge", 923),
        (
            "channel-negated-reply",
            "in:alice,supinic !from:carol is:reply",
            455,
        ),
        (
            "regex-filter",
            r#"regex:"clip merge candidate https://example\.com/\d+""#,
            1_000,
        ),
        ("badge-subtier", "badge:moderator subtier:3", 295),
        ("wiki-is-deleted", "is:deleted", 50),
        ("wiki-is-disabled", "is:disabled", 50),
        ("wiki-is-sub", "is:sub", 264),
        ("wiki-is-subscription", "is:subscription", 264),
        ("wiki-is-timeout", "is:timeout", 173),
        ("wiki-is-ban", "is:ban", 173),
        ("wiki-is-highlighted", "is:highlighted", 52),
        ("wiki-is-system", "is:system", 162),
        ("wiki-is-first-msg", "is:first-msg", 95),
        ("wiki-is-cheer-msg", "is:cheer-msg", 218),
        ("wiki-is-redemption", "is:redemption", 136),
        ("wiki-badge-no-video", "badge:no_video", 122),
    ] {
        let compiled = compile_search_query(query, false).unwrap();
        let actual = messages.iter().filter(|msg| compiled.matches(*msg)).count();
        assert_eq!(actual, expected, "match count mismatch for {name}");
    }

    assert!(
        compile_search_query(r#"regex:"[""#, false).is_err(),
        "invalid regex filter should fail"
    );
}
