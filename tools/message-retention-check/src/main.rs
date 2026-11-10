mod config {
    pub const MAX_CACHED_MESSAGES: usize = 1_000;
    pub const MAX_INACTIVE_CACHED_MESSAGES: usize = 1_000;
}

#[path = "../../../src/state/message_retention.rs"]
mod message_retention;

use message_retention::{
    RetentionClass, active_channel_capacity, inactive_channel_capacity, plan_for_message_count,
};

fn parse_config_const(name: &str) -> usize {
    let source = include_str!("../../../src/config.rs");
    let needle = format!("pub const {name}: usize = ");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {name} in src/config.rs"))
        + needle.len();
    let end = source[start..]
        .find(';')
        .unwrap_or_else(|| panic!("unterminated {name} const"))
        + start;
    source[start..end]
        .trim()
        .replace('_', "")
        .parse()
        .unwrap_or_else(|error| panic!("failed to parse {name}: {error}"))
}

fn main() {
    assert_eq!(
        config::MAX_CACHED_MESSAGES,
        parse_config_const("MAX_CACHED_MESSAGES"),
        "checker active retention constant drifted from src/config.rs"
    );
    assert_eq!(
        config::MAX_INACTIVE_CACHED_MESSAGES,
        parse_config_const("MAX_INACTIVE_CACHED_MESSAGES"),
        "checker inactive retention constant drifted from src/config.rs"
    );

    let active_capacity = active_channel_capacity();
    let inactive_capacity = inactive_channel_capacity();

    assert!(
        active_capacity >= 1,
        "active retention capacity must be non-zero"
    );
    assert!(
        inactive_capacity >= 1,
        "inactive retention capacity must be non-zero"
    );
    assert!(
        inactive_capacity <= active_capacity,
        "inactive retention must not exceed active retention"
    );

    println!("class,input,target_capacity,retained,dropped_oldest");
    for active in [true, false] {
        for input in [
            0usize,
            inactive_capacity.saturating_sub(1),
            inactive_capacity,
            inactive_capacity + 1,
            active_capacity.saturating_sub(1),
            active_capacity,
            active_capacity + 1,
            active_capacity * 5,
        ] {
            let plan = plan_for_message_count(input, active);
            let expected_capacity = if active {
                active_capacity
            } else {
                inactive_capacity
            };

            assert_eq!(plan.target_capacity, expected_capacity);
            assert_eq!(plan.retained_messages, input.min(expected_capacity));
            assert_eq!(
                plan.dropped_oldest_messages,
                input.saturating_sub(plan.retained_messages)
            );

            let class = match plan.class {
                RetentionClass::Active => "active",
                RetentionClass::Inactive => "inactive",
            };
            println!(
                "{class},{},{},{},{}",
                plan.input_messages,
                plan.target_capacity,
                plan.retained_messages,
                plan.dropped_oldest_messages
            );
        }
    }
}
