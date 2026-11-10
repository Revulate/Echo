use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

#[macro_export]
macro_rules! log_irc {
    ($level:ident, $fmt:literal $(, $arg:expr_2021)* $(,)?) => {{
        let _ = stringify!($level);
        let _ = format_args!($fmt $(, $arg)*);
    }};
}

mod backend {
    use super::*;

    pub mod lock_utils {
        pub use echo_core::sync::MutexExt;
    }

    #[derive(Clone, Debug, Default)]
    pub struct ChatMessage {
        pub id: Arc<str>,
        pub channel: Arc<str>,
        pub sender: Arc<str>,
        pub display_name: Arc<str>,
        pub content: Arc<str>,
    }

    pub mod observers {
        use super::ChatMessage;

        // The decoration variants exist so the included observer's matches
        // compile; this check never sends them.
        #[allow(dead_code)]
        #[derive(Clone, Debug)]
        pub enum ChannelEvent {
            NewMessage(ChatMessage),
            UserDecorationsChanged {},
            BadgeDecorationsChanged,
        }

        pub trait ChannelObserver {
            fn on_event(&self, channel: &str, event: ChannelEvent);
        }
    }
}

#[path = "../../../src/state/chat_observer.rs"]
mod chat_observer;
#[path = "../../../src/state/chat_observer_recovery.rs"]
mod chat_observer_recovery;

use backend::observers::{ChannelEvent, ChannelObserver};
use chat_observer::ChatStateObserver;
use chat_observer_recovery::{ObserverDrainAction, observer_drain_action};

fn main() {
    let observer = ChatStateObserver::new("overflow_channel".to_string());
    let notify_count = Arc::new(AtomicU32::new(0));
    let notify_count_for_callback = Arc::clone(&notify_count);
    observer.set_notify_callback(move || {
        notify_count_for_callback.fetch_add(1, Ordering::Relaxed);
    });

    for i in 0..5_000 {
        let msg = backend::ChatMessage {
            id: Arc::from(format!("msg_{i}")),
            ..backend::ChatMessage::default()
        };
        observer.on_event("overflow_channel", ChannelEvent::NewMessage(msg));
    }

    assert!(observer.has_pending_events());
    let drained = observer.drain_events();
    let action = observer_drain_action(drained.dropped_count);
    let retained_id_bytes: usize = drained
        .events
        .iter()
        .map(|event| match event {
            ChannelEvent::NewMessage(msg) => {
                msg.id.len()
                    + msg.channel.len()
                    + msg.sender.len()
                    + msg.display_name.len()
                    + msg.content.len()
            }
            ChannelEvent::UserDecorationsChanged { .. } | ChannelEvent::BadgeDecorationsChanged => {
                0
            }
        })
        .sum();

    assert!(
        drained.dropped_count > 0,
        "smoke must force observer queue overflow"
    );
    assert!(
        !drained.events.is_empty(),
        "overflow should retain the bounded newest event window"
    );
    assert!(
        retained_id_bytes > 0,
        "retained event payloads should remain inspectable after bounded overflow"
    );
    assert!(
        !drained.dropped_decoration_event,
        "only message events were queued, so no dropped event was a decoration change"
    );
    assert!(
        drained.oldest_wait.is_some(),
        "a non-empty drain must report how long its oldest event waited"
    );
    assert_eq!(
        action,
        ObserverDrainAction::RefreshFromBackendSnapshot,
        "dropped observer events must force snapshot recovery"
    );
    assert_ne!(
        action,
        ObserverDrainAction::ApplyQueuedEvents,
        "partial event queues must not be applied after overflow"
    );

    println!("case,queued_events,dropped_events,notify_events,action");
    println!(
        "observer_overflow_recovery,{},{},{},refresh_from_backend_snapshot",
        drained.events.len(),
        drained.dropped_count,
        notify_count.load(Ordering::Relaxed)
    );
}
