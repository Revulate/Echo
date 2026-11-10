//! Contract: whisper-thread summaries, ordering, unread transitions and
//! sent/received formatting in `src/backend/whispers.rs`. Harness stubs intentionally do not
//! validate bounded message-buffer capacity, slot parsing/hashing, observer delivery,
//! retokenization, or frame-wakeup effects; those require their owning integration
//! boundaries.

mod state {
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
    }

    pub mod ui_invalidation {
        #[derive(Clone, Copy, Debug)]
        pub enum UiInvalidation {
            TopBarState,
        }
        pub fn request(_: UiInvalidation) {}
    }
}

// Stubs for the `crate::backend` items `whispers.rs` uses. Some variants and the
// blocked-user policy are only reached from paths this check does not drive.
#[allow(dead_code)]
mod backend {
    pub mod activity_persistence {
        pub fn mark_dirty() {}
        pub fn retire_account_visibility_commit_held(
            _commit: &crate::backend::blocked_users::VisibilityCommit,
        ) {
        }
    }

    pub mod lock_utils {
        pub use echo_core::sync::RwLockExt;
    }

    pub mod models {
        pub use echo_core::message::*;
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
        ) -> Vec<echo_core::slot::MessageSlot> {
            Vec::new()
        }
        pub use echo_core::slot::compute_structure_hash;
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
        use std::sync::LazyLock;
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
        pub struct VisibilityCommit {
            _gate: std::sync::MutexGuard<'static, ()>,
        }

        pub fn visibility_commit_guard() -> VisibilityCommit {
            static GATE: std::sync::LazyLock<std::sync::Mutex<()>> =
                std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
            VisibilityCommit {
                _gate: GATE.lock().expect("whisper fixture gate poisoned"),
            }
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

#[path = "../../../src/backend/whispers.rs"]
#[allow(dead_code)] // account-clear, activity and blocked-user paths are not driven here
mod whispers;

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};

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

    println!("{FIXTURE_HEADER}");
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
    assert_fixture(
        "whisper_last_received_sender",
        whispers::last_received_sender().unwrap_or_default(),
        "Bob",
    );
    assert_fixture("whisper_thread_count", threads.len().to_string(), "2");
    assert_fixture("whisper_latest_username", &threads[0].username, "Bob");
    assert_fixture("whisper_latest_message", &threads[0].last_message, "ping");
    assert_fixture(
        "whisper_latest_unread_count",
        threads[0].unread_count.to_string(),
        "1",
    );
    assert_fixture(
        "whisper_latest_timestamp",
        threads[0].last_timestamp.to_string(),
        "30",
    );
    assert_fixture("whisper_older_username", &threads[1].username, "alice");
    assert_fixture(
        "whisper_older_message",
        &threads[1].last_message,
        "You: reply",
    );
    assert_fixture(
        "whisper_older_unread_count",
        threads[1].unread_count.to_string(),
        "0",
    );
    assert_fixture(
        "whisper_older_timestamp",
        threads[1].last_timestamp.to_string(),
        "20",
    );
    assert_fixture(
        "whisper_initial_unread_count",
        initial_unread_count.to_string(),
        "1",
    );
    assert_fixture(
        "whisper_mark_read_case_insensitive",
        bool_text(mark_alice_read),
        "false",
    );
    assert_fixture(
        "whisper_unread_after_mark_read",
        unread_after_mark_read.to_string(),
        "1",
    );
    assert_fixture(
        "whisper_mark_missing_read",
        bool_text(mark_missing_read),
        "false",
    );
    assert_fixture("whisper_mark_all_read", bool_text(mark_all_read), "true");
    assert_fixture(
        "whisper_unread_after_mark_all_read",
        unread_after_mark_all_read.to_string(),
        "0",
    );
    assert_fixture(
        "whisper_mark_all_read_again",
        bool_text(mark_all_read_again),
        "false",
    );
}
