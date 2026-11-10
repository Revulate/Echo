//! Contract: quick-switcher ranking (`echo_core::quick_switch`) with
//! echo-core's `fuzzy_score`: exact beats prefix beats camel-case matches,
//! case-insensitively; ties break on recent channels, then live channels, then
//! name.

use check_support::{FIXTURE_HEADER, assert_fixture};
use echo_core::fuzzy::fuzzy_score;
use echo_core::quick_switch::{QuickSwitchCandidate, rank_channels};

#[derive(Clone, Debug)]
struct QuickItem {
    channel: &'static str,
    is_live: bool,
}

fn main() {
    println!("{FIXTURE_HEADER}");

    let exact = fuzzy_score("pog", "pog").expect("exact match");
    let prefix = fuzzy_score("pog", "PogChamp").expect("prefix match");
    let camel = fuzzy_score("pc", "PogChamp").expect("camel-case match");
    let case_a = fuzzy_score("pog", "PogChamp").expect("lowercase query");
    let case_b = fuzzy_score("POG", "pogchamp").expect("uppercase query");

    assert!(exact > prefix, "exact match should outrank prefix match");
    assert!(
        prefix > camel,
        "prefix match should outrank sparse camel match"
    );
    assert_eq!(case_a, case_b, "fuzzy matching should be case-insensitive");
    assert!(fuzzy_score("xyz", "PogChamp").is_none());

    assert_fixture(
        "fuzzy_exact_gt_prefix",
        ordering_text(exact > prefix),
        "true",
    );
    assert_fixture(
        "fuzzy_prefix_gt_camel",
        ordering_text(prefix > camel),
        "true",
    );
    assert_fixture(
        "fuzzy_case_insensitive",
        ordering_text(case_a == case_b),
        "true",
    );
    assert_fixture(
        "fuzzy_no_match",
        option_text(fuzzy_score("xyz", "PogChamp")),
        "none",
    );

    let items = [
        QuickItem {
            channel: "zeta",
            is_live: false,
        },
        QuickItem {
            channel: "alpha",
            is_live: false,
        },
        QuickItem {
            channel: "beta",
            is_live: true,
        },
        QuickItem {
            channel: "gamma",
            is_live: true,
        },
    ];
    let recent = ["zeta", "alpha"];

    assert_fixture(
        "quick_switch_empty_recent_live_alpha",
        ordered_channels("", &items, &recent),
        "zeta|alpha|beta|gamma",
    );
    assert_fixture(
        "quick_switch_query_exact_first",
        ordered_channels("beta", &items, &recent),
        "beta",
    );
    assert_fixture(
        "quick_switch_query_prefix_before_sparse",
        ordered_channels(
            "ga",
            &[
                QuickItem {
                    channel: "greatalpha",
                    is_live: false,
                },
                QuickItem {
                    channel: "gamma",
                    is_live: false,
                },
            ],
            &[],
        ),
        "gamma|greatalpha",
    );
}

fn ordered_channels(query: &str, items: &[QuickItem], recent: &[&str]) -> String {
    let candidates: Vec<QuickSwitchCandidate<'_>> = items
        .iter()
        .map(|item| QuickSwitchCandidate {
            channel: item.channel,
            mention_count: 0,
            is_live: item.is_live,
        })
        .collect();
    rank_channels(query, &candidates, recent)
        .into_iter()
        .map(|idx| items[idx].channel)
        .collect::<Vec<_>>()
        .join("|")
}

fn ordering_text(value: bool) -> String {
    value.to_string()
}

fn option_text(value: Option<f64>) -> &'static str {
    if value.is_some() { "some" } else { "none" }
}
