#![allow(dead_code)]

mod lock_utils {
    pub use echo_core::sync::RwLockExt;
}

mod backend {
    pub mod settings {
        use serde::{Deserialize, Serialize};
        use std::sync::{LazyLock, Mutex};

        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
        pub struct FilterRule {
            pub id: String,
            pub expression: String,
            pub action: FilterAction,
            pub enabled: bool,
        }

        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
        pub enum FilterAction {
            Allow,
            Hide,
            #[default]
            Highlight,
            Replace,
        }

        static FILTERS: LazyLock<Mutex<Vec<FilterRule>>> = LazyLock::new(|| Mutex::new(Vec::new()));

        pub fn get_filters() -> Vec<FilterRule> {
            FILTERS
                .lock()
                .map(|filters| filters.clone())
                .unwrap_or_default()
        }

        pub fn set_filters(filters: Vec<FilterRule>) {
            if let Ok(mut stored) = FILTERS.lock() {
                *stored = filters;
            }
        }
    }
}

use backend::settings;
#[path = "../../../src/backend/filters.rs"]
mod filters;
use filters::{FILTER_STORE, FilterAction, TestContext, validate_filter_expression};
use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Copy)]
struct FilterCase {
    name: &'static str,
    expression: &'static str,
    expected: bool,
}

const FILTER_CASES: &[FilterCase] = &[
    FilterCase {
        name: "content_badge",
        expression: r#"(author.badges contains "moderator") && (message.content contains "merge")"#,
        expected: true,
    },
    FilterCase {
        name: "message_length_or_subbed",
        expression: r#"message.length < 80 || author.subbed"#,
        expected: true,
    },
    FilterCase {
        name: "channel_negation",
        expression: r#"channel.name == "somestreamer" && !message.content contains "spoiler""#,
        expected: true,
    },
    FilterCase {
        name: "regex_url",
        expression: r#"message.content match r"https://example\.com/\d+""#,
        expected: true,
    },
    FilterCase {
        name: "regex_capture_group",
        expression: r#"(message.content match {r"(\d\d)/(\d\d)/(\d\d\d\d)", 3}) == "2020""#,
        expected: true,
    },
    FilterCase {
        name: "arithmetic_precedence",
        expression: "2 + 3 * 4 == 20",
        expected: true,
    },
    FilterCase {
        name: "cheer_bits_flag",
        expression: r#"bits.amount >= 100 && flags.cheer_message"#,
        expected: true,
    },
    FilterCase {
        name: "channel_reward_state",
        expression: r#"channel.watching && channel.live && reward.title == "Hydrate""#,
        expected: true,
    },
    FilterCase {
        name: "external_badge_reward_cost",
        expression: r#"author.external_badges contains "7tv:artist" && reward.cost >= 500"#,
        expected: true,
    },
    FilterCase {
        name: "returning_not_first",
        expression: r#"flags.returning_chatter && !flags.first_message"#,
        expected: true,
    },
    FilterCase {
        name: "sub_watch_streak",
        expression: r#"flags.sub_message && flags.watch_streak"#,
        expected: true,
    },
    FilterCase {
        name: "author_no_color_false",
        expression: r#"author.no_color"#,
        expected: false,
    },
];

fn main() {
    let ctx = TestContext {
        content: "hello 12/31/2020 merge candidate https://example.com/42".to_string(),
        author: "SomeViewer".to_string(),
        author_login: "someviewer".to_string(),
        author_user_id: "1234".to_string(),
        author_color: Some("#FF0000".to_string()),
        badges: vec!["moderator".to_string(), "subscriber".to_string()],
        external_badges: vec!["7tv:artist".to_string()],
        sub_length: Some(12),
        channel_name: "somestreamer".to_string(),
        bits_amount: 100,
        reward_title: "Hydrate".to_string(),
        reward_cost: 500,
        reward_id: "reward-123".to_string(),
        is_sub: true,
        is_cheer: true,
        is_returning_chatter: true,
        is_sub_message: true,
        is_watch_streak: true,
        is_reward_message: true,
        channel_watching: true,
        channel_live: true,
        ..Default::default()
    };

    for (index, case) in FILTER_CASES.iter().enumerate() {
        assert_filter_case(&ctx, index, case);
    }

    assert!(validate_filter_expression("").is_err());
    assert!(validate_filter_expression(r#"message.content contains"#).is_err());
    assert!(validate_filter_expression(r#"message.content match r"(""#).is_err());

    let benchmark_filters = vec![
        settings::FilterRule {
            id: "content-link".to_string(),
            expression: r#"message.content contains "merge" && message.content match r"https://example\.com/\d+""#
                .to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "badge-sub".to_string(),
            expression: r#"author.badges contains "moderator" && author.sub_length >= 6"#.to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "capture".to_string(),
            expression: r#"(message.content match {r"(\d\d)/(\d\d)/(\d\d\d\d)", 3}) == "2020""#
                .to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "cheer".to_string(),
            expression: r#"bits.amount >= 100 && flags.cheer_message"#.to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "channel-state".to_string(),
            expression: r#"channel.watching && channel.live && reward.id == "reward-123""#
                .to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "returning-chatter".to_string(),
            expression: r#"flags.returning_chatter && author.login == "someviewer""#.to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
        settings::FilterRule {
            id: "sub-watch-streak".to_string(),
            expression: r#"flags.sub_message && flags.watch_streak"#.to_string(),
            action: settings::FilterAction::Highlight,
            enabled: true,
        },
    ];
    let filter_count = benchmark_filters.len();
    settings::set_filters(benchmark_filters);

    black_box(FILTER_STORE.test_message(&ctx));

    let iterations = 250_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let results = FILTER_STORE.test_message(black_box(&ctx));
        checksum = checksum.wrapping_add(results.iter().filter(|result| result.matched).count());
    }
    let elapsed = start.elapsed();
    let units = iterations * filter_count;
    let ns_per_unit = elapsed.as_nanos() as f64 / units as f64;
    let units_per_second = units as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(&ctx);
    println!(
        "match,filter_expr,{},{:.2},{:.0},{}",
        units, ns_per_unit, units_per_second, checksum
    );
    println!(
        "action_map,hide={},highlight={},allow={}",
        action_id(FilterAction::Hide),
        action_id(FilterAction::Highlight),
        action_id(FilterAction::Allow)
    );
}

fn assert_filter_case(ctx: &TestContext, index: usize, case: &FilterCase) {
    let matched = evaluate_filter_case(ctx, &format!("case-{index}"), case.expression);
    assert_eq!(
        matched, case.expected,
        "expression {} returned {matched}, expected {}",
        case.name, case.expected
    );
}

fn evaluate_filter_case(ctx: &TestContext, id: &str, expression: &str) -> bool {
    validate_filter_expression(expression)
        .unwrap_or_else(|error| panic!("expression should validate: {expression}: {error}"));
    settings::set_filters(vec![settings::FilterRule {
        id: id.to_string(),
        expression: expression.to_string(),
        action: settings::FilterAction::Highlight,
        enabled: true,
    }]);

    FILTER_STORE
        .test_message(ctx)
        .first()
        .map(|result| result.matched)
        .unwrap_or(false)
}

fn print_smoke_rows(ctx: &TestContext) {
    for case in FILTER_CASES {
        let matched = evaluate_filter_case(ctx, &format!("fixture-{}", case.name), case.expression);
        print_fixture(
            &format!("filter_case_{}", case.name),
            bool_text(matched),
            bool_text(case.expected),
        );
    }

    print_fixture(
        "filter_invalid_empty_rejected",
        bool_text(validate_filter_expression("").is_err()),
        "true",
    );
    print_fixture(
        "filter_invalid_missing_rhs_rejected",
        bool_text(validate_filter_expression(r#"message.content contains"#).is_err()),
        "true",
    );
    print_fixture(
        "filter_invalid_regex_rejected",
        bool_text(validate_filter_expression(r#"message.content match r"(""#).is_err()),
        "true",
    );
    print_fixture("filter_action_hide", action_id(FilterAction::Hide), "hide");
    print_fixture(
        "filter_action_highlight",
        action_id(FilterAction::Highlight),
        "highlight",
    );
    print_fixture(
        "filter_action_allow",
        action_id(FilterAction::Allow),
        "allow",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn action_id(action: FilterAction) -> &'static str {
    match action {
        FilterAction::Allow => "allow",
        FilterAction::Hide => "hide",
        FilterAction::Highlight => "highlight",
        FilterAction::Replace => "replace",
    }
}
