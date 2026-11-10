#![allow(dead_code)]

//! Narrow deterministic coverage for whisper-thread summaries, ordering, unread
//! transitions, and sent/received formatting. Harness stubs intentionally do not
//! validate bounded message-buffer capacity, slot parsing/hashing, observer delivery,
//! retokenization, or frame-wakeup effects; those require their owning integration
//! boundaries.

mod state {
    use std::sync::Arc;

    #[derive(Clone, Debug, Default)]
    pub struct MessageBuffer {
        messages: Vec<crate::backend::models::ChatMessage>,
    }
    impl MessageBuffer {
        pub fn new(_capacity: usize) -> Self {
            Self::default()
        }
        pub fn push_stamped(&mut self, message: &mut crate::backend::models::ChatMessage) {
            self.messages.push(message.clone());
        }
        pub fn retain<F>(&mut self, mut keep: F) -> usize
        where
            F: FnMut(&crate::backend::models::ChatMessage) -> bool,
        {
            let before = self.messages.len();
            self.messages.retain(|message| keep(message));
            before - self.messages.len()
        }
        #[allow(dead_code)]
        pub fn snapshot(&mut self) -> Arc<Vec<crate::backend::models::ChatMessage>> {
            Arc::new(self.messages.clone())
        }
    }

    pub mod ui_invalidation {
        #[derive(Clone, Copy, Debug)]
        pub enum UiInvalidation {
            TopBarState,
        }
        pub fn request(_: UiInvalidation) {}
    }
}

mod backend {
    pub mod activity_persistence {
        pub fn mark_dirty() {}
        pub fn retire_account_visibility_commit_held() {}
    }

    pub mod lock_utils {
        pub use echo_core::sync::RwLockExt;
    }

    pub mod models {
        use std::sync::Arc;
        #[derive(Clone, Debug, Default)]
        pub struct ChatMessage {
            pub id: Arc<str>,
            pub channel: Arc<str>,
            pub emote_lookup_channel: Option<Arc<str>>,
            pub emote_lookup_source_id: Option<Arc<str>>,
            pub sender: Arc<str>,
            pub display_name: Arc<str>,
            pub content: Arc<str>,
            pub color: Option<Arc<str>>,
            pub badges: Vec<Arc<str>>,
            pub timestamp: i64,
            pub slots: Arc<[()]>,
            pub structure_hash: u64,
            pub reply: Option<()>,
            pub flags: MessageFlags,
            pub twitch_emotes_tag: Option<Arc<str>>,
            pub twitch_gifs_tag: Option<Arc<str>>,
            pub user_id: Option<Arc<str>>,
            pub reply_emote_offset: usize,
        }
        #[derive(Clone, Debug, Default)]
        pub struct MessageFlags {
            pub highlight_type: Option<HighlightType>,
        }
        #[derive(Clone, Debug)]
        pub enum HighlightType {
            Whisper,
        }
    }

    pub mod auth {
        pub fn active_authenticated_user_id() -> Option<String> {
            None
        }
    }

    pub mod settings {
        pub fn get_username() -> Option<String> {
            None
        }
    }

    pub mod emotes {
        pub struct EmoteSender;

        impl EmoteSender {
            pub fn new(_user_id: Option<&str>, _login: &str) -> Self {
                Self
            }
        }
    }

    pub mod slots {
        use rustc_hash::FxHashMap;
        pub fn parse_to_slots(
            _message: &str,
            _channel: &str,
            _sender: super::emotes::EmoteSender,
            _emotes: &FxHashMap<usize, (String, usize)>,
        ) -> Vec<()> {
            Vec::new()
        }
        pub fn compute_structure_hash(_slots: &[()]) -> u64 {
            0
        }
    }

    pub mod irc {
        use dashmap::DashMap;
        use std::sync::LazyLock;
        pub static CHAT_MESSAGES: LazyLock<DashMap<String, crate::state::MessageBuffer>> =
            LazyLock::new(DashMap::new);
        pub fn bump_chat_version() {}
    }

    pub mod observers {
        use super::models::ChatMessage;
        use std::sync::{Arc, LazyLock};
        #[derive(Clone)]
        #[allow(clippy::large_enum_variant)]
        pub enum ChannelEvent {
            NewMessage(ChatMessage),
            MessagesRetokenized,
        }
        pub struct Registry;
        impl Registry {
            pub fn notify(&self, _channel: &str, _event: ChannelEvent) {}
        }
        static REGISTRY: LazyLock<Registry> = LazyLock::new(|| Registry);
        pub fn get_observer_registry() -> &'static Registry {
            &REGISTRY
        }
        #[allow(dead_code)]
        fn _arc(_: Arc<str>) {}
    }

    // Minimal stub of backend::blocked_users for the #[path]-included
    // whispers.rs; visibility is never revoked in the smoke fixtures.
    pub mod blocked_users {
        pub fn is_blocked(_user_id: Option<&str>, _user_login: &str) -> bool {
            false
        }

        pub struct BlockedVisibilityPolicy;
        impl BlockedVisibilityPolicy {
            pub fn is_empty(&self) -> bool {
                true
            }
            pub fn contains(&self, _user_id: Option<&str>, _user_login: &str) -> bool {
                false
            }
        }
        pub fn resident_visibility_revoked() -> bool {
            false
        }
        pub fn visibility_policy_snapshot() -> Option<BlockedVisibilityPolicy> {
            None
        }
        pub fn visibility_commit_guard() -> std::sync::MutexGuard<'static, ()> {
            static GATE: std::sync::LazyLock<std::sync::Mutex<()>> =
                std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
            GATE.lock().expect("whisper fixture gate poisoned")
        }
    }

    pub mod types {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct WhisperThread {
            pub username: String,
            pub last_message: String,
            pub unread_count: u32,
            pub last_timestamp: i64,
        }
    }
}

mod managers {
    pub mod frame_rate_controller {
        pub fn wakeup() {}
        pub fn record_activity() {}
    }
}

impl state::MessageBuffer {
    // Keep the production path's private buffer type available at crate::state::MessageBuffer.
}

#[path = "../../../src/backend/whispers.rs"]
#[allow(clippy::needless_update)]
mod whispers;

use std::hint::black_box;
use std::time::Instant;

fn main() {
    whispers::record_received("Alice", "hello", 10);
    whispers::record_sent("alice", "alice-id", "reply", 20);
    whispers::record_received("Bob", "ping", 30);

    assert_eq!(whispers::last_received_sender().as_deref(), Some("Bob"));

    let threads = whispers::get_threads();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].username, "Bob");
    assert_eq!(threads[0].last_message, "ping");
    assert_eq!(threads[0].unread_count, 1);
    assert_eq!(threads[0].last_timestamp, 30);
    assert_eq!(threads[1].username, "alice");
    assert_eq!(threads[1].last_message, "You: reply");
    assert_eq!(threads[1].unread_count, 0);
    assert_eq!(threads[1].last_timestamp, 20);
    let initial_unread_count = whispers::unread_count();
    assert_eq!(initial_unread_count, 1);
    let mark_alice_read = whispers::mark_read("ALICE");
    assert!(!mark_alice_read);
    let unread_after_mark_read = whispers::unread_count();
    assert_eq!(unread_after_mark_read, 1);
    let mark_missing_read = whispers::mark_read("missing");
    assert!(!mark_missing_read);
    let mark_all_read = whispers::mark_all_read();
    assert!(mark_all_read);
    assert_eq!(whispers::unread_count(), 0);
    let mark_all_read_again = whispers::mark_all_read();
    assert!(!mark_all_read_again);
    let unread_after_mark_all_read = whispers::unread_count();

    let iterations = 250_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for index in 0..iterations {
        let user = if index & 1 == 0 { "Alice" } else { "Bob" };
        whispers::record_received(
            black_box(user),
            black_box("message"),
            black_box(index as i64),
        );
        whispers::record_sent(
            black_box(user),
            black_box("fixture-user-id"),
            black_box("ack"),
            black_box(index as i64),
        );
        checksum = checksum.wrapping_add(whispers::get_threads().len());
    }
    let elapsed = start.elapsed();
    let units = iterations * 2;
    let ns_per_unit = elapsed.as_nanos() as f64 / units as f64;
    let units_per_second = units as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(WhisperSmokeRows {
        threads: &threads,
        initial_unread_count,
        mark_alice_read,
        unread_after_mark_read,
        mark_missing_read,
        mark_all_read,
        unread_after_mark_all_read,
        mark_all_read_again,
    });
    println!(
        "track,whisper_threads,{},{:.2},{:.0},{}",
        units, ns_per_unit, units_per_second, checksum
    );
}

struct WhisperSmokeRows<'a> {
    threads: &'a [backend::types::WhisperThread],
    initial_unread_count: u32,
    mark_alice_read: bool,
    unread_after_mark_read: u32,
    mark_missing_read: bool,
    mark_all_read: bool,
    unread_after_mark_all_read: u32,
    mark_all_read_again: bool,
}

fn print_smoke_rows(rows: WhisperSmokeRows<'_>) {
    let WhisperSmokeRows {
        threads,
        initial_unread_count,
        mark_alice_read,
        unread_after_mark_read,
        mark_missing_read,
        mark_all_read,
        unread_after_mark_all_read,
        mark_all_read_again,
    } = rows;
    print_fixture(
        "whisper_last_received_sender",
        &whispers::last_received_sender().unwrap_or_default(),
        "Bob",
    );
    print_fixture("whisper_thread_count", &threads.len().to_string(), "2");
    print_fixture("whisper_latest_username", &threads[0].username, "Bob");
    print_fixture("whisper_latest_message", &threads[0].last_message, "ping");
    print_fixture(
        "whisper_latest_unread_count",
        &threads[0].unread_count.to_string(),
        "1",
    );
    print_fixture(
        "whisper_latest_timestamp",
        &threads[0].last_timestamp.to_string(),
        "30",
    );
    print_fixture("whisper_older_username", &threads[1].username, "alice");
    print_fixture(
        "whisper_older_message",
        &threads[1].last_message,
        "You: reply",
    );
    print_fixture(
        "whisper_older_unread_count",
        &threads[1].unread_count.to_string(),
        "0",
    );
    print_fixture(
        "whisper_older_timestamp",
        &threads[1].last_timestamp.to_string(),
        "20",
    );
    print_fixture(
        "whisper_initial_unread_count",
        &initial_unread_count.to_string(),
        "1",
    );
    print_fixture(
        "whisper_mark_read_case_insensitive",
        bool_text(mark_alice_read),
        "false",
    );
    print_fixture(
        "whisper_unread_after_mark_read",
        &unread_after_mark_read.to_string(),
        "1",
    );
    print_fixture(
        "whisper_mark_missing_read",
        bool_text(mark_missing_read),
        "false",
    );
    print_fixture("whisper_mark_all_read", bool_text(mark_all_read), "true");
    print_fixture(
        "whisper_unread_after_mark_all_read",
        &unread_after_mark_all_read.to_string(),
        "0",
    );
    print_fixture(
        "whisper_mark_all_read_again",
        bool_text(mark_all_read_again),
        "false",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
