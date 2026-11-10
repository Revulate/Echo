//! Embed expansion state management
//!
//! Tracks which embeds are expanded (showing full preview/video)
//! vs collapsed (showing thumbnail preview).

use crate::backend::link_types::ResourceKey;
use dashmap::DashMap;
use std::sync::LazyLock;

/// Global expansion state store
/// Key format: "{message_id}:{embed_id}"
static EXPANSION_STATE: LazyLock<EmbedExpansionState> = LazyLock::new(EmbedExpansionState::new);

/// A rendered occurrence of a resource. A repeated URL may share a resource
/// key but disclosure is always local to this message/embed occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewOccurrence {
    pub message_id: String,
    pub embed_id: u32,
    pub resource_key: ResourceKey,
}

/// Manages per-embed expansion state
pub struct EmbedExpansionState {
    /// Tracks which embeds are currently expanded
    expanded: DashMap<PreviewOccurrence, bool>,
}

impl EmbedExpansionState {
    /// Create new expansion state tracker
    pub fn new() -> Self {
        Self {
            expanded: DashMap::new(),
        }
    }

    /// Check if a specific embed is expanded
    pub fn is_expanded(&self, occurrence: &PreviewOccurrence) -> bool {
        self.expanded.get(occurrence).map(|v| *v).unwrap_or(false)
    }

    pub fn set_expanded(&self, occurrence: PreviewOccurrence, expanded: bool) {
        if expanded {
            self.expanded.insert(occurrence, true);
        } else {
            self.expanded.remove(&occurrence);
        }
    }
}

impl Default for EmbedExpansionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a specific embed is expanded
pub fn is_embed_expanded(occurrence: &PreviewOccurrence) -> bool {
    EXPANSION_STATE.is_expanded(occurrence)
}

pub fn set_embed_expanded(occurrence: PreviewOccurrence, expanded: bool) {
    EXPANSION_STATE.set_expanded(occurrence, expanded);
}

/// Drop disclosure state when its backing message leaves a bounded buffer.
pub fn prune_message_id(message_id: &str) {
    EXPANSION_STATE
        .expanded
        .retain(|occurrence, _| occurrence.message_id != message_id);
}
