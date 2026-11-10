//! Contract: USERNOTICE classification in `src/backend/usernotice_core.rs`: event
//! labels, subscription and watch-streak detection, highlight kinds and the
//! system-message slots.

mod backend {
    pub mod slots {
        use std::sync::Arc;

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum MessageSlot {
            Text(Arc<str>),
            Mention { username: Arc<str> },
        }

        impl MessageSlot {
            pub fn text(text: impl Into<Arc<str>>) -> Self {
                Self::Text(text.into())
            }
        }
    }
}

#[path = "../../../src/backend/usernotice_core.rs"]
mod usernotice_core;

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};
use usernotice_core::{UsernoticeHighlightKind, UsernoticeTags};

fn main() {
    use backend::slots::MessageSlot;

    assert_eq!(
        usernotice_core::usernotice_system_slots("Thanks Alice and Bob!", &["Alice", "Bob"]),
        vec![
            MessageSlot::text("Thanks "),
            MessageSlot::Mention {
                username: "Alice".into(),
            },
            MessageSlot::text(" and "),
            MessageSlot::Mention {
                username: "Bob".into(),
            },
            MessageSlot::text("!"),
        ]
    );

    for msg_id in [
        "sub",
        "resub",
        "subgift",
        "anonsubgift",
        "submysterygift",
        "giftpaidupgrade",
        "anongiftpaidupgrade",
        "primepaidupgrade",
        "communitypayforward",
    ] {
        assert!(usernotice_core::is_subscription_usernotice(msg_id));
    }
    assert!(!usernotice_core::is_subscription_usernotice("raid"));

    assert_eq!(usernotice_core::event_type_label("sub"), "subscription");
    assert_eq!(
        usernotice_core::event_type_label("submysterygift"),
        "gift subscription"
    );
    assert_eq!(
        usernotice_core::event_type_label("giftpaidupgrade"),
        "subscription upgrade"
    );
    assert_eq!(
        usernotice_core::event_type_label("anongiftpaidupgrade"),
        "subscription upgrade"
    );
    assert_eq!(
        usernotice_core::event_type_label("primepaidupgrade"),
        "subscription upgrade"
    );
    assert_eq!(
        usernotice_core::event_type_label("communitypayforward"),
        "pay it forward"
    );
    assert_eq!(
        usernotice_core::event_type_label("bitsbadgetier"),
        "bits badge"
    );
    assert_eq!(
        usernotice_core::event_type_label("watch-streak"),
        "watch streak"
    );
    assert_eq!(usernotice_core::event_type_label("unknown"), "notice");

    for msg_id in [
        "sub",
        "resub",
        "subgift",
        "anonsubgift",
        "submysterygift",
        "giftpaidupgrade",
        "anongiftpaidupgrade",
        "primepaidupgrade",
        "communitypayforward",
    ] {
        assert_eq!(
            usernotice_core::highlight_kind(&tags(msg_id)),
            Some(UsernoticeHighlightKind::Subscription)
        );
    }
    assert_eq!(
        usernotice_core::highlight_kind(&tags("raid")),
        Some(UsernoticeHighlightKind::Raid)
    );
    assert_eq!(
        usernotice_core::highlight_kind(&tags("announcement")),
        Some(UsernoticeHighlightKind::Announcement)
    );
    assert_eq!(
        usernotice_core::highlight_kind(&tags("bitsbadgetier")),
        Some(UsernoticeHighlightKind::BitsBadge)
    );

    let mut watch = tags("ritual");
    watch.ritual_name = Some("watch-streak".to_string());
    assert!(usernotice_core::is_watch_streak_usernotice(&watch));
    assert_eq!(
        usernotice_core::highlight_kind(&watch),
        Some(UsernoticeHighlightKind::WatchStreak)
    );

    let mut months = tags("ritual");
    months.watch_streak_months = Some(3);
    assert!(usernotice_core::is_watch_streak_usernotice(&months));

    let mut flag = tags("ritual");
    flag.watch_streak = Some("true".to_string());
    assert!(usernotice_core::is_watch_streak_usernotice(&flag));
    assert!(usernotice_core::is_watch_streak_usernotice(&tags(
        "watch-streak"
    )));
    assert!(usernotice_core::is_watch_streak_usernotice(&tags(
        "watchstreak"
    )));

    assert_eq!(usernotice_core::highlight_kind(&tags("ritual")), None);

    println!("{FIXTURE_HEADER}");
    print_smoke_rows();
}

fn print_smoke_rows() {
    for (name, msg_id, expected_label) in [
        ("usernotice_label_sub", "sub", "subscription"),
        (
            "usernotice_label_submysterygift",
            "submysterygift",
            "gift subscription",
        ),
        (
            "usernotice_label_giftpaidupgrade",
            "giftpaidupgrade",
            "subscription upgrade",
        ),
        (
            "usernotice_label_communitypayforward",
            "communitypayforward",
            "pay it forward",
        ),
        (
            "usernotice_label_bitsbadgetier",
            "bitsbadgetier",
            "bits badge",
        ),
        (
            "usernotice_label_watch_streak",
            "watch-streak",
            "watch streak",
        ),
        ("usernotice_label_unknown", "unknown", "notice"),
    ] {
        assert_fixture(
            name,
            usernotice_core::event_type_label(msg_id),
            expected_label,
        );
    }

    for (name, msg_id, expected) in [
        ("usernotice_sub_highlight", "sub", "subscription"),
        ("usernotice_raid_highlight", "raid", "raid"),
        (
            "usernotice_announcement_highlight",
            "announcement",
            "announcement",
        ),
        ("usernotice_bits_highlight", "bitsbadgetier", "bits badge"),
        ("usernotice_ritual_no_highlight", "ritual", "none"),
    ] {
        let actual = highlight_name(usernotice_core::highlight_kind(&tags(msg_id)));
        assert_fixture(name, actual, expected);
    }

    let mut ritual = tags("ritual");
    ritual.ritual_name = Some("watch-streak".to_string());
    assert_fixture(
        "usernotice_watch_streak_ritual_name",
        bool_text(usernotice_core::is_watch_streak_usernotice(&ritual)),
        "true",
    );

    let mut months = tags("ritual");
    months.watch_streak_months = Some(3);
    assert_fixture(
        "usernotice_watch_streak_months",
        bool_text(usernotice_core::is_watch_streak_usernotice(&months)),
        "true",
    );

    let mut flag = tags("ritual");
    flag.watch_streak = Some("true".to_string());
    assert_fixture(
        "usernotice_watch_streak_flag",
        bool_text(usernotice_core::is_watch_streak_usernotice(&flag)),
        "true",
    );

    assert_fixture(
        "usernotice_watch_streak_msg_id",
        bool_text(usernotice_core::is_watch_streak_usernotice(&tags(
            "watch-streak",
        ))),
        "true",
    );
    assert_fixture(
        "usernotice_watchstreak_msg_id",
        bool_text(usernotice_core::is_watch_streak_usernotice(&tags(
            "watchstreak",
        ))),
        "true",
    );
}

fn highlight_name(kind: Option<UsernoticeHighlightKind>) -> &'static str {
    match kind {
        Some(UsernoticeHighlightKind::Subscription) => "subscription",
        Some(UsernoticeHighlightKind::Raid) => "raid",
        Some(UsernoticeHighlightKind::Announcement) => "announcement",
        Some(UsernoticeHighlightKind::BitsBadge) => "bits badge",
        Some(UsernoticeHighlightKind::WatchStreak) => "watch streak",
        None => "none",
    }
}

fn tags(msg_id: &str) -> UsernoticeTags {
    UsernoticeTags {
        msg_id: msg_id.to_string(),
        ..UsernoticeTags::default()
    }
}
