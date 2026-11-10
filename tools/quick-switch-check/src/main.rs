use std::cmp::Ordering;

use echo_core::fuzzy_score;

#[derive(Clone, Debug)]
struct QuickItem {
    channel: &'static str,
    is_live: bool,
}

fn main() {
    println!("kind,name,value,checksum");

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

    print_fixture(
        "fuzzy_exact_gt_prefix",
        &ordering_text(exact > prefix),
        "true",
    );
    print_fixture(
        "fuzzy_prefix_gt_camel",
        &ordering_text(prefix > camel),
        "true",
    );
    print_fixture(
        "fuzzy_case_insensitive",
        &ordering_text(case_a == case_b),
        "true",
    );
    print_fixture(
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

    print_fixture(
        "quick_switch_empty_recent_live_alpha",
        &ordered_channels("", &items, &recent),
        "zeta|alpha|beta|gamma",
    );
    print_fixture(
        "quick_switch_query_exact_first",
        &ordered_channels("beta", &items, &recent),
        "beta",
    );
    print_fixture(
        "quick_switch_query_prefix_before_sparse",
        &ordered_channels(
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
    let mut indices: Vec<usize> = (0..items.len()).collect();

    if query.is_empty() {
        indices.sort_by(|a_idx, b_idx| {
            let a = &items[*a_idx];
            let b = &items[*b_idx];
            recent_rank(recent, a.channel)
                .cmp(&recent_rank(recent, b.channel))
                .then_with(|| b.is_live.cmp(&a.is_live))
                .then_with(|| a.channel.cmp(b.channel))
        });
    } else {
        let mut matches: Vec<(usize, f64)> = indices
            .into_iter()
            .filter_map(|idx| fuzzy_score(query, items[idx].channel).map(|score| (idx, score)))
            .collect();

        matches.sort_by(|(a_idx, a_score), (b_idx, b_score)| {
            let a = &items[*a_idx];
            let b = &items[*b_idx];
            b_score
                .partial_cmp(a_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| recent_rank(recent, a.channel).cmp(&recent_rank(recent, b.channel)))
                .then_with(|| b.is_live.cmp(&a.is_live))
                .then_with(|| a.channel.cmp(b.channel))
        });
        indices = matches.into_iter().map(|(idx, _)| idx).collect();
    }

    indices
        .iter()
        .map(|idx| items[*idx].channel)
        .collect::<Vec<_>>()
        .join("|")
}

fn recent_rank(recent: &[&str], channel: &str) -> usize {
    recent
        .iter()
        .position(|recent| recent.eq_ignore_ascii_case(channel))
        .unwrap_or(usize::MAX)
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn ordering_text(value: bool) -> String {
    value.to_string()
}

fn option_text(value: Option<f64>) -> &'static str {
    if value.is_some() { "some" } else { "none" }
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
