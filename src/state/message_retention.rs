//! Shared message-retention sizing for backend buffers and UI snapshots.

use crate::config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionClass {
    Active,
    Inactive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageRetentionPlan {
    pub class: RetentionClass,
    pub input_messages: usize,
    pub target_capacity: usize,
    pub retained_messages: usize,
    pub dropped_oldest_messages: usize,
}

pub fn active_channel_capacity() -> usize {
    config::MAX_CACHED_MESSAGES.max(1)
}

pub fn inactive_channel_capacity() -> usize {
    config::MAX_INACTIVE_CACHED_MESSAGES
        .min(active_channel_capacity())
        .max(1)
}

pub fn target_capacity_for(active: bool) -> usize {
    if active {
        active_channel_capacity()
    } else {
        inactive_channel_capacity()
    }
}

pub fn plan_for_message_count(input_messages: usize, active: bool) -> MessageRetentionPlan {
    let target_capacity = target_capacity_for(active);
    let retained_messages = input_messages.min(target_capacity);
    MessageRetentionPlan {
        class: if active {
            RetentionClass::Active
        } else {
            RetentionClass::Inactive
        },
        input_messages,
        target_capacity,
        retained_messages,
        dropped_oldest_messages: input_messages.saturating_sub(retained_messages),
    }
}
