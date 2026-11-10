//! GPUI-free emote preference model.
//!
//! Holds the persisted shape (favorites, recent usage, usage frequency,
//! collapsed categories) and the in-memory policy around it. Durable storage
//! and the debounced background save worker stay in the application, reached
//! through [`EmotePreferencesPorts`]; without an installed set the model is a
//! plain in-memory store.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

/// Maximum number of favorite emotes
pub const MAX_FAVORITES: usize = 50;

/// Maximum number of recently used emotes to track
pub const MAX_RECENT: usize = 25;

/// Debounce interval the application's background save worker applies after
/// a mutation (milliseconds).
pub const SAVE_DEBOUNCE_MS: u128 = 500;

static PORTS: OnceLock<EmotePreferencesPorts> = OnceLock::new();

/// Storage hooks the model cannot reach on its own.
#[derive(Clone, Copy)]
pub struct EmotePreferencesPorts {
    /// Write a preferences snapshot to durable storage.
    pub persist: fn(&EmotePreferencesData) -> Result<(), Box<dyn std::error::Error>>,
    /// Request a debounced background save of the current preferences.
    pub schedule_save: fn(),
}

/// Install the application's preference storage hooks. Idempotent.
pub fn install_emote_preferences_ports(ports: EmotePreferencesPorts) {
    let _ = PORTS.set(ports);
}

fn persist(data: &EmotePreferencesData) -> Result<(), Box<dyn std::error::Error>> {
    match PORTS.get() {
        Some(ports) => (ports.persist)(data),
        None => Ok(()),
    }
}

fn schedule_save() {
    if let Some(ports) = PORTS.get() {
        (ports.schedule_save)();
    }
}

/// Reference to an emote (for persistence without storing full data)
#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct EmoteRef {
    pub id: String,
    pub provider: String, // "Twitch", "SevenTV", "BTTV", "FFZ"
    pub name: String,
}

impl EmoteRef {
    pub fn new(id: &str, provider: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            provider: provider.to_string(),
            name: name.to_string(),
        }
    }

    /// Generate a unique key for this emote
    pub fn key(&self) -> String {
        format!("{}:{}", self.id, self.provider)
    }
}

/// Persisted emote preferences data
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EmotePreferencesData {
    /// Favorite emotes (max 50)
    pub favorites: Vec<EmoteRef>,
    /// Recently used emotes (most recent first, max 25)
    #[serde(default)]
    pub recently_used: VecDeque<EmoteRef>,
    /// Usage frequency (emote_key -> count)
    #[serde(default)]
    pub usage_counts: HashMap<String, u32>,
    /// Collapsed category IDs
    #[serde(default)]
    pub collapsed_categories: HashSet<String>,
    /// Schema version for future migrations
    #[serde(default)]
    pub version: u32,
}

/// Runtime emote preferences with change tracking
pub struct EmotePreferences {
    pub data: EmotePreferencesData,
    /// Dirty flag for debounced saving
    dirty: bool,
    /// Identifies the snapshot so a completed background save cannot clear a newer mutation.
    revision: u64,
    /// Set of favorite keys for O(1) lookup
    favorites_set: HashSet<String>,
}

impl Default for EmotePreferences {
    fn default() -> Self {
        Self::new()
    }
}

impl EmotePreferences {
    /// Create new empty preferences
    pub fn new() -> Self {
        Self {
            data: EmotePreferencesData::default(),
            dirty: false,
            revision: 0,
            favorites_set: HashSet::new(),
        }
    }

    /// Adopt a snapshot loaded from storage as the clean current state.
    pub fn from_data(data: EmotePreferencesData) -> Self {
        let favorites_set = data.favorites.iter().map(|e| e.key()).collect();
        Self {
            data,
            dirty: false,
            revision: 0,
            favorites_set,
        }
    }

    /// Whether there are mutations that have not been persisted yet.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Monotonic mutation counter, used to detect a newer mutation racing a
    /// background save that started from an older snapshot.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Record that the current state has reached durable storage.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Save preferences to disk
    pub fn save(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        persist(&self.data)?;
        self.dirty = false;
        crate::log_emotes!(debug, "Saved emote preferences");
        Ok(())
    }

    /// Try to save if needed (debounced)
    pub fn try_save(&mut self) {
        if self.dirty {
            schedule_save();
        }
    }

    /// Mark as dirty (needs save)
    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    // ========================================================================
    // Favorites Management
    // ========================================================================

    /// Check if an emote is a favorite
    pub fn is_favorite(&self, id: &str, provider: &str) -> bool {
        let key = format!("{}:{}", id, provider);
        self.favorites_set.contains(&key)
    }

    /// Get reference to the favorites set (for efficient cloning in render loops)
    pub fn favorites_set(&self) -> &HashSet<String> {
        &self.favorites_set
    }

    /// Toggle favorite status for an emote
    pub fn toggle_favorite(&mut self, id: &str, provider: &str, name: &str) -> bool {
        let key = format!("{}:{}", id, provider);
        let is_now_favorite = if self.favorites_set.contains(&key) {
            // Remove from favorites
            self.favorites_set.remove(&key);
            self.data.favorites.retain(|e| e.key() != key);
            false
        } else {
            // Add to favorites (if under limit)
            if self.data.favorites.len() < MAX_FAVORITES {
                let emote_ref = EmoteRef::new(id, provider, name);
                self.favorites_set.insert(key);
                self.data.favorites.push(emote_ref);
                true
            } else {
                crate::log_emotes!(warn, "Maximum favorites ({}) reached", MAX_FAVORITES);
                false
            }
        };
        self.mark_dirty();
        is_now_favorite
    }

    // ========================================================================
    // Usage Tracking
    // ========================================================================

    /// Record emote usage (updates recent and frequency)
    pub fn record_usage(&mut self, id: &str, provider: &str, name: &str) {
        let key = format!("{}:{}", id, provider);

        // Update frequency count
        *self.data.usage_counts.entry(key).or_insert(0) += 1;

        // Update recent list (remove if exists, add to front)
        let emote_ref = EmoteRef::new(id, provider, name);
        self.data
            .recently_used
            .retain(|e| !(e.id == id && e.provider == provider));
        self.data.recently_used.push_front(emote_ref);

        // Trim to max size
        while self.data.recently_used.len() > MAX_RECENT {
            self.data.recently_used.pop_back();
        }

        self.mark_dirty();
    }

    /// Get recently used emote refs
    pub fn get_recent(&self, count: usize) -> Vec<&EmoteRef> {
        self.data.recently_used.iter().take(count).collect()
    }

    /// Get usage count for an emote
    pub fn get_usage_count(&self, id: &str, provider: &str) -> u32 {
        let key = format!("{}:{}", id, provider);
        self.data.usage_counts.get(&key).copied().unwrap_or(0)
    }

    // ========================================================================
    // Category Collapse State
    // ========================================================================

    /// Check if a category is collapsed
    pub fn is_category_collapsed(&self, category_id: &str) -> bool {
        self.data.collapsed_categories.contains(category_id)
    }

    /// Toggle category collapsed state
    pub fn toggle_category_collapsed(&mut self, category_id: &str) {
        if self.data.collapsed_categories.contains(category_id) {
            self.data.collapsed_categories.remove(category_id);
        } else {
            self.data
                .collapsed_categories
                .insert(category_id.to_string());
        }
        self.mark_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emote_ref_key() {
        let emote = EmoteRef::new("123", "Twitch", "Kappa");
        assert_eq!(emote.key(), "123:Twitch");
    }

    #[test]
    fn test_favorites() {
        let mut prefs = EmotePreferences::new();

        // Add favorite
        assert!(prefs.toggle_favorite("123", "Twitch", "Kappa"));
        assert!(prefs.is_favorite("123", "Twitch"));

        // Toggle off
        assert!(!prefs.toggle_favorite("123", "Twitch", "Kappa"));
        assert!(!prefs.is_favorite("123", "Twitch"));

        // Toggle back on
        assert!(prefs.toggle_favorite("123", "Twitch", "Kappa"));
        assert!(prefs.is_favorite("123", "Twitch"));
    }

    #[test]
    fn test_recent_usage() {
        let mut prefs = EmotePreferences::new();

        prefs.record_usage("1", "Twitch", "Kappa");
        prefs.record_usage("2", "SevenTV", "KEKW");
        prefs.record_usage("1", "Twitch", "Kappa");

        let recent = prefs.get_recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].name, "Kappa"); // Most recent

        assert_eq!(prefs.get_usage_count("1", "Twitch"), 2);
        assert_eq!(prefs.get_usage_count("2", "SevenTV"), 1);
    }
}
