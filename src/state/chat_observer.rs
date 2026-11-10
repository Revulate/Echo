//! GPUI-compatible chat observer for reactive state updates.
//!
//! Bridges between the backend observer system and GPUI's entity/context system.

use crate::backend::lock_utils::MutexExt;
use crate::backend::observers::{ChannelEvent, ChannelObserver};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Wakes the GPUI side after an event is queued; called from background threads.
type NotifyCallback = Arc<dyn Fn() + Send + Sync>;

/// Hard cap to prevent unbounded memory growth when UI thread stalls.
const MAX_EVENT_QUEUE_LEN: usize = 4096;

/// Observer that queues events for processing on the next GPUI frame
pub struct ChatStateObserver {
    channel: String,
    /// Queue of events to process (accessed from background threads)
    event_queue: Mutex<VecDeque<ChannelEvent>>,
    /// When the queue last became non-empty. Locked only while holding
    /// `event_queue`.
    oldest_queued_at: Mutex<Option<Instant>>,
    /// Callback to trigger GPUI notification (called from background threads)
    notify_callback: Mutex<Option<NotifyCallback>>,
    /// Number of dropped events due to queue backpressure.
    dropped_events: AtomicU64,
    /// Whether any dropped event was a badge or user decoration change.
    dropped_decoration_event: AtomicBool,
}

pub struct DrainedEvents {
    pub events: Vec<ChannelEvent>,
    pub dropped_count: u64,
    pub dropped_decoration_event: bool,
    /// How long the oldest drained event waited in the queue.
    pub oldest_wait: Option<Duration>,
}

impl ChatStateObserver {
    pub fn new(channel: String) -> Arc<Self> {
        Arc::new(Self {
            channel,
            event_queue: Mutex::new(VecDeque::new()),
            oldest_queued_at: Mutex::new(None),
            notify_callback: Mutex::new(None),
            dropped_events: AtomicU64::new(0),
            dropped_decoration_event: AtomicBool::new(false),
        })
    }

    /// Set the callback that triggers GPUI re-render
    /// Must be called from GPUI context
    pub fn set_notify_callback<F>(&self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut cb = self.notify_callback.lock_or_recover();
        *cb = Some(Arc::new(callback));
    }

    /// Check if there are pending events
    pub fn has_pending_events(&self) -> bool {
        let queue = self.event_queue.lock_or_recover();
        !queue.is_empty()
    }

    /// Drain all pending events (for reactive processing)
    /// Returns owned Vec of events, clearing the queue
    pub fn drain_events(&self) -> DrainedEvents {
        let mut queue = self.event_queue.lock_or_recover();
        let events: Vec<ChannelEvent> = queue.drain(..).collect();
        let oldest_wait = self
            .oldest_queued_at
            .lock_or_recover()
            .take()
            .map(|queued_at| queued_at.elapsed());
        let dropped = self.dropped_events.swap(0, Ordering::Relaxed);
        let dropped_decoration_event = self.dropped_decoration_event.swap(false, Ordering::Relaxed);
        if dropped > 0 {
            crate::log_irc!(
                warn,
                "ChatStateObserver '{}' dropped {} events due to backpressure",
                self.channel,
                dropped
            );
        }
        DrainedEvents {
            events,
            dropped_count: dropped,
            dropped_decoration_event,
            oldest_wait,
        }
    }
}

impl ChannelObserver for ChatStateObserver {
    fn on_event(&self, _channel: &str, event: ChannelEvent) {
        let queue_len = {
            let mut queue = self.event_queue.lock_or_recover();
            if queue.len() >= MAX_EVENT_QUEUE_LEN {
                if let Some(dropped) = queue.pop_front()
                    && matches!(
                        dropped,
                        ChannelEvent::UserDecorationsChanged { .. }
                            | ChannelEvent::BadgeDecorationsChanged
                    )
                {
                    self.dropped_decoration_event.store(true, Ordering::Relaxed);
                }
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(event);
            if queue.len() == 1 {
                *self.oldest_queued_at.lock_or_recover() = Some(Instant::now());
            }
            queue.len()
        };

        crate::log_irc!(
            trace,
            "ChatStateObserver::on_event for '{}' - queue now has {} events",
            self.channel,
            queue_len
        );

        let callback = self.notify_callback.lock_or_recover().clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatMessage;
    use crate::backend::observers::ChannelObserver;
    use core::prelude::v1::test;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn test_message(id: &str) -> ChatMessage {
        ChatMessage {
            id: Arc::from(id),
            channel: Arc::from("test_channel"),
            sender: Arc::from("testuser"),
            display_name: Arc::from("TestUser"),
            content: Arc::from("Hello"),
            ..ChatMessage::default()
        }
    }

    #[test]
    fn test_event_queuing() {
        let observer = ChatStateObserver::new("test_channel".to_string());

        observer.on_event(
            "test_channel",
            ChannelEvent::NewMessage(test_message("test1")),
        );

        assert!(observer.has_pending_events());
        let queue = observer.event_queue.lock_or_recover();
        assert_eq!(queue.len(), 1);
        drop(queue);

        let drained = observer.drain_events();
        assert_eq!(drained.events.len(), 1);
        assert_eq!(drained.dropped_count, 0);
        assert!(!observer.has_pending_events());
    }

    #[test]
    fn test_backpressure_reports_dropped_events() {
        let observer = ChatStateObserver::new("test_channel".to_string());
        let overflow = 16usize;

        for i in 0..(MAX_EVENT_QUEUE_LEN + overflow) {
            let msg = test_message(&format!("msg_{i}"));
            observer.on_event("test_channel", ChannelEvent::NewMessage(msg));
        }

        let drained = observer.drain_events();
        assert_eq!(drained.events.len(), MAX_EVENT_QUEUE_LEN);
        assert_eq!(drained.dropped_count, overflow as u64);
        assert!(!drained.dropped_decoration_event);
    }

    #[test]
    fn dropped_decoration_event_is_reported() {
        let observer = ChatStateObserver::new("test_channel".to_string());
        observer.on_event("test_channel", ChannelEvent::BadgeDecorationsChanged);
        for i in 0..MAX_EVENT_QUEUE_LEN {
            let msg = test_message(&format!("msg_{i}"));
            observer.on_event("test_channel", ChannelEvent::NewMessage(msg));
        }

        let drained = observer.drain_events();
        assert_eq!(drained.dropped_count, 1);
        assert!(drained.dropped_decoration_event);
        assert!(!observer.drain_events().dropped_decoration_event);
    }

    #[test]
    fn event_notifies_after_it_is_queued() {
        let observer = ChatStateObserver::new("test_channel".to_string());
        let notifications = Arc::new(AtomicUsize::new(0));
        let callback_notifications = notifications.clone();
        observer.set_notify_callback(move || {
            callback_notifications.fetch_add(1, Ordering::Relaxed);
        });

        observer.on_event(
            "test_channel",
            ChannelEvent::NewMessage(test_message("test1")),
        );

        assert_eq!(notifications.load(Ordering::Relaxed), 1);
        assert!(observer.has_pending_events());
    }
}
