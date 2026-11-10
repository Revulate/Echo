//! Serializable disk-cache model and index policy.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DISPLAYED_EMOTE_RETENTION_SECS: u64 = 3 * 24 * 60 * 60;
pub const DISK_CACHE_VERSION: u32 = 1;
pub const SOURCE_CACHE_VERSION: u32 = 1;

/// Type of cached content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryType {
    /// Static image (PNG, non-animated WebP)
    Static,
    /// Animated image (GIF, animated WebP)
    Animated,
}

/// A single cache entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Hash of the URL (used as filename/directory name)
    pub hash: String,
    /// Type of content
    pub entry_type: EntryType,
    /// Size on disk in bytes
    pub size_bytes: u64,
    /// Last access timestamp (Unix seconds)
    pub last_accessed: u64,
    /// Access count (for LFU scoring)
    pub access_count: u32,
    /// Image width
    pub width: u32,
    /// Image height
    pub height: u32,
    /// Frame count (1 for static images)
    pub frame_count: u16,
    /// Total animation duration in ms (0 for static)
    pub total_duration_ms: u32,
}

impl CacheEntry {
    /// Create a new static image entry
    pub fn new_static(hash: String, size_bytes: u64, width: u32, height: u32) -> Self {
        Self {
            hash,
            entry_type: EntryType::Static,
            size_bytes,
            last_accessed: current_timestamp(),
            access_count: 1,
            width,
            height,
            frame_count: 1,
            total_duration_ms: 0,
        }
    }

    /// Calculate eviction score (lower = evict first)
    pub fn eviction_score(&self) -> f64 {
        let now = current_timestamp();
        let recency_secs = now.saturating_sub(self.last_accessed) as f64;
        let frequency = (self.access_count as f64).ln().max(1.0);

        // Higher frequency and lower recency = higher score = keep longer
        frequency / (recency_secs / 3600.0 + 1.0)
    }

    /// Touch the entry (update access time and count)
    pub fn touch(&mut self) {
        self.last_accessed = current_timestamp();
        self.access_count = self.access_count.saturating_add(1);
    }
}

/// Persistent source media key for emotes whose provider identity is stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaSourceId {
    pub provider: Arc<str>,
    pub id: Arc<str>,
    pub variant: MediaVariant,
}

impl MediaSourceId {
    pub fn new(
        provider: impl Into<Arc<str>>,
        id: impl Into<Arc<str>>,
        variant: MediaVariant,
    ) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            variant,
        }
    }

    pub fn new_shared_labels(provider: &str, id: Arc<str>, variant: MediaVariant) -> Self {
        Self {
            provider: shared_media_label(provider),
            id,
            variant,
        }
    }

    pub fn key_string(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.provider,
            self.id,
            self.variant.scale,
            self.variant.format,
            self.variant.animated_kind.as_str()
        )
    }
}

/// Provider source variant. This distinguishes resolution and static/animated
/// assets for providers that can serve different bytes for one emote ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaVariant {
    pub scale: Arc<str>,
    pub format: Arc<str>,
    pub animated_kind: MediaAnimatedKind,
}

impl MediaVariant {
    pub fn new(
        scale: impl Into<Arc<str>>,
        format: impl Into<Arc<str>>,
        animated_kind: MediaAnimatedKind,
    ) -> Self {
        Self {
            scale: scale.into(),
            format: format.into(),
            animated_kind,
        }
    }

    pub fn for_emote_url(provider: &str, scale: &str, url: &str, is_animated: bool) -> Self {
        let lower = if url.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(url.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(url)
        };
        let path = lower.split('?').next().unwrap_or(lower.as_ref());
        let format = path
            .rsplit_once('.')
            .map(|(_, extension)| extension.trim())
            .filter(|extension| !extension.is_empty() && extension.len() <= 8)
            .map(shared_media_label)
            .unwrap_or_else(|| {
                let fallback = match provider {
                    "twitch" if lower.contains("/animated/") || is_animated => "animated",
                    "twitch" => "static",
                    "bttv" => "webp",
                    "ffz" => "image",
                    "7tv" if is_animated => "webp",
                    "7tv" => "image",
                    "emoji" => "png",
                    _ => "image",
                };
                shared_media_label(fallback)
            });
        let animated_url = lower.ends_with(".gif")
            || lower.contains("/animated/")
            || lower.contains("_animated.")
            || lower.contains("format=gif");

        Self {
            scale: shared_media_label(scale),
            format,
            animated_kind: if is_animated || animated_url {
                MediaAnimatedKind::Animated
            } else {
                MediaAnimatedKind::Static
            },
        }
    }
}

static SHARED_MEDIA_LABELS: LazyLock<[Arc<str>; 25]> = LazyLock::new(|| {
    [
        "twitch", "7tv", "bttv", "ffz", "emoji", "1.0", "2.0", "3.0", "1", "2", "4", "1x", "2x",
        "3x", "4x", "default", "animated", "static", "webp", "image", "png", "gif", "jpg", "jpeg",
        "avif",
    ]
    .map(Arc::from)
});

fn shared_media_label(value: &str) -> Arc<str> {
    let index = match value {
        "twitch" => 0,
        "7tv" => 1,
        "bttv" => 2,
        "ffz" => 3,
        "emoji" => 4,
        "1.0" => 5,
        "2.0" => 6,
        "3.0" => 7,
        "1" => 8,
        "2" => 9,
        "4" => 10,
        "1x" => 11,
        "2x" => 12,
        "3x" => 13,
        "4x" => 14,
        "default" => 15,
        "animated" => 16,
        "static" => 17,
        "webp" => 18,
        "image" => 19,
        "png" => 20,
        "gif" => 21,
        "jpg" => 22,
        "jpeg" => 23,
        "avif" => 24,
        _ => return Arc::from(value),
    };
    Arc::clone(&SHARED_MEDIA_LABELS[index])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaAnimatedKind {
    Static,
    Animated,
    Unknown,
}

impl MediaAnimatedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Animated => "animated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceBlobEncoding {
    Raw,
    Zstd,
}

impl SourceBlobEncoding {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Zstd => "zst",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBlobEntry {
    pub hash: String,
    pub encoding: SourceBlobEncoding,
    pub size_bytes: u64,
    pub original_size_bytes: u64,
    pub last_accessed: u64,
    pub access_count: u32,
    pub width: u32,
    pub height: u32,
    pub frame_count: u16,
    pub total_duration_ms: u32,
}

impl SourceBlobEntry {
    pub fn new(
        hash: String,
        encoding: SourceBlobEncoding,
        size_bytes: u64,
        original_size_bytes: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            hash,
            encoding,
            size_bytes,
            original_size_bytes,
            last_accessed: current_timestamp(),
            access_count: 1,
            width,
            height,
            frame_count: 1,
            total_duration_ms: 0,
        }
    }

    pub fn touch(&mut self) {
        self.last_accessed = current_timestamp();
        self.access_count = self.access_count.saturating_add(1);
    }

    pub fn eviction_score(&self) -> f64 {
        let now = current_timestamp();
        let recency_secs = now.saturating_sub(self.last_accessed) as f64;
        let frequency = (self.access_count as f64).ln().max(1.0);
        frequency / (recency_secs / 3600.0 + 1.0)
    }

    pub fn as_cache_entry(&self) -> CacheEntry {
        CacheEntry {
            hash: self.hash.clone(),
            entry_type: EntryType::Static,
            size_bytes: self.size_bytes,
            last_accessed: self.last_accessed,
            access_count: self.access_count,
            width: self.width,
            height: self.height,
            frame_count: self.frame_count,
            total_duration_ms: self.total_duration_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAlias {
    pub blob_hash: String,
    pub last_accessed: u64,
    pub access_count: u32,
}

impl SourceAlias {
    pub fn new(blob_hash: String) -> Self {
        Self {
            blob_hash,
            last_accessed: current_timestamp(),
            access_count: 1,
        }
    }

    pub fn touch(&mut self) {
        self.last_accessed = current_timestamp();
        self.access_count = self.access_count.saturating_add(1);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializableSourceIndex {
    pub version: u32,
    pub identity_aliases: Vec<(String, SourceAlias)>,
    pub url_aliases: Vec<(String, SourceAlias)>,
    pub blobs: Vec<(String, SourceBlobEntry)>,
    pub total_size_bytes: u64,
}

#[derive(Debug)]
pub struct SourceCacheIndex {
    pub version: u32,
    pub identity_aliases: IndexMap<String, SourceAlias>,
    pub url_aliases: IndexMap<String, SourceAlias>,
    pub blobs: IndexMap<String, SourceBlobEntry>,
    pub total_size_bytes: u64,
}

impl SourceCacheIndex {
    pub fn new() -> Self {
        Self {
            version: SOURCE_CACHE_VERSION,
            identity_aliases: IndexMap::new(),
            url_aliases: IndexMap::new(),
            blobs: IndexMap::new(),
            total_size_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    pub fn get_alias_blob_hash(&self, identity_key: Option<&str>, url: &str) -> Option<String> {
        identity_key
            .and_then(|key| self.identity_aliases.get(key))
            .or_else(|| self.url_aliases.get(url))
            .map(|alias| alias.blob_hash.clone())
    }

    pub fn touch_aliases_for_blob(
        &mut self,
        blob_hash: &str,
        identity_key: Option<&str>,
        url: &str,
    ) {
        if let Some(key) = identity_key
            && let Some(alias) = self.identity_aliases.get_mut(key)
        {
            alias.touch();
        }
        if let Some(alias) = self.url_aliases.get_mut(url) {
            alias.touch();
        }
        if let Some(blob) = self.blobs.get_mut(blob_hash) {
            blob.touch();
        }
    }

    pub fn insert_blob(&mut self, blob: SourceBlobEntry) -> bool {
        let hash = blob.hash.clone();
        if let Some(old) = self.blobs.get(&hash) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(old.size_bytes);
        }
        let is_new = !self.blobs.contains_key(&hash);
        self.total_size_bytes = self.total_size_bytes.saturating_add(blob.size_bytes);
        self.blobs.insert(hash, blob);
        is_new
    }

    pub fn insert_aliases(&mut self, identity_key: Option<String>, url: String, blob_hash: String) {
        if let Some(key) = identity_key {
            self.identity_aliases
                .insert(key, SourceAlias::new(blob_hash.clone()));
        }
        self.url_aliases.insert(url, SourceAlias::new(blob_hash));
    }

    pub fn remove_blob(&mut self, blob_hash: &str) -> Option<SourceBlobEntry> {
        let removed = self.blobs.swap_remove(blob_hash)?;
        self.total_size_bytes = self.total_size_bytes.saturating_sub(removed.size_bytes);
        self.identity_aliases
            .retain(|_, alias| alias.blob_hash != blob_hash);
        self.url_aliases
            .retain(|_, alias| alias.blob_hash != blob_hash);
        Some(removed)
    }

    pub fn remove_url_alias(&mut self, url: &str) -> Option<String> {
        self.url_aliases
            .swap_remove(url)
            .map(|alias| alias.blob_hash)
    }

    pub fn remove_identity_alias(&mut self, identity_key: &str) -> Option<String> {
        self.identity_aliases
            .swap_remove(identity_key)
            .map(|alias| alias.blob_hash)
    }

    pub fn blob_alias_count(&self, blob_hash: &str) -> usize {
        self.identity_aliases
            .values()
            .filter(|alias| alias.blob_hash == blob_hash)
            .count()
            + self
                .url_aliases
                .values()
                .filter(|alias| alias.blob_hash == blob_hash)
                .count()
    }

    fn is_blob_retained(&self, blob_hash: &str, now: u64) -> bool {
        if let Some(blob) = self.blobs.get(blob_hash)
            && now.saturating_sub(blob.last_accessed) <= DISPLAYED_EMOTE_RETENTION_SECS
        {
            return true;
        }

        self.identity_aliases
            .values()
            .chain(self.url_aliases.values())
            .any(|alias| {
                alias.blob_hash == blob_hash
                    && now.saturating_sub(alias.last_accessed) <= DISPLAYED_EMOTE_RETENTION_SECS
            })
    }
    pub fn get_eviction_candidates(&self, bytes_needed: u64) -> Vec<(String, SourceBlobEntry)> {
        let now = current_timestamp();
        // Retention is an eviction-priority tier, not immunity: non-retained
        // blobs go first, but recently-accessed ones still follow when the
        // shortfall demands it. With retention as a hard filter the byte
        // budget was unenforceable whenever the whole working set was fresh
        // (observed at 181% of budget with every eviction pass freeing 0).
        let mut scored: Vec<_> = self
            .blobs
            .iter()
            .map(|(hash, entry)| {
                (
                    hash.clone(),
                    entry.clone(),
                    self.is_blob_retained(hash, now),
                    entry.eviction_score(),
                )
            })
            .collect();
        scored.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
        });

        let mut total = 0u64;
        let mut candidates = Vec::new();
        for (hash, entry, _, _) in scored {
            if total >= bytes_needed {
                break;
            }
            total += entry.size_bytes;
            candidates.push((hash, entry));
        }
        candidates
    }
}

impl Default for SourceCacheIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl From<SerializableSourceIndex> for SourceCacheIndex {
    fn from(s: SerializableSourceIndex) -> Self {
        Self {
            version: s.version,
            identity_aliases: s.identity_aliases.into_iter().collect(),
            url_aliases: s.url_aliases.into_iter().collect(),
            blobs: s.blobs.into_iter().collect(),
            total_size_bytes: s.total_size_bytes,
        }
    }
}

impl From<&SourceCacheIndex> for SerializableSourceIndex {
    fn from(index: &SourceCacheIndex) -> Self {
        Self {
            version: index.version,
            identity_aliases: index
                .identity_aliases
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            url_aliases: index
                .url_aliases
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            blobs: index
                .blobs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            total_size_bytes: index.total_size_bytes,
        }
    }
}

/// Serializable index format
#[derive(Debug, Serialize, Deserialize)]
pub struct SerializableIndex {
    pub version: u32,
    pub entries: Vec<(String, CacheEntry)>,
    pub total_size_bytes: u64,
}

/// Disk cache index
#[derive(Debug)]
pub struct DiskCacheIndex {
    /// Cache version
    pub version: u32,
    /// URL → entry mapping
    pub entries: IndexMap<String, CacheEntry>,
    /// Total size of all entries
    pub total_size_bytes: u64,
}

impl DiskCacheIndex {
    /// Create a new empty index
    pub fn new() -> Self {
        Self {
            version: DISK_CACHE_VERSION,
            entries: IndexMap::new(),
            total_size_bytes: 0,
        }
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get entry for a URL
    pub fn get(&self, url: &str) -> Option<&CacheEntry> {
        self.entries.get(url)
    }

    /// Touch an entry (update access time, move to end for LRU)
    pub fn touch(&mut self, url: &str) -> bool {
        if let Some(entry) = self.entries.get_mut(url) {
            entry.touch();
            if let Some(idx) = self.entries.get_index_of(url) {
                self.entries.move_index(idx, self.entries.len() - 1);
            }
            true
        } else {
            false
        }
    }

    /// Insert or update an entry
    pub fn insert(&mut self, url: String, entry: CacheEntry) {
        if let Some(old) = self.entries.get(&url) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(old.size_bytes);
        }
        self.total_size_bytes += entry.size_bytes;
        self.entries.insert(url, entry);
    }

    /// Remove an entry
    pub fn remove(&mut self, url: &str) -> Option<CacheEntry> {
        if let Some(entry) = self.entries.swap_remove(url) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(entry.size_bytes);
            Some(entry)
        } else {
            None
        }
    }

    /// Get entries to evict to free the specified number of bytes.
    ///
    /// Recently-accessed entries are a lower-priority eviction tier, not
    /// immune — see SourceCacheIndex::get_eviction_candidates.
    pub fn get_eviction_candidates(&self, bytes_needed: u64) -> Vec<(String, CacheEntry)> {
        let now = current_timestamp();
        let mut scored: Vec<_> = self
            .entries
            .iter()
            .map(|(url, entry)| {
                (
                    url.clone(),
                    entry.clone(),
                    now.saturating_sub(entry.last_accessed) <= DISPLAYED_EMOTE_RETENTION_SECS,
                    entry.eviction_score(),
                )
            })
            .collect();

        scored.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
        });

        let mut total = 0u64;
        let mut candidates = Vec::new();

        for (url, entry, _, _score) in scored {
            if total >= bytes_needed {
                break;
            }
            total += entry.size_bytes;
            candidates.push((url, entry));
        }

        candidates
    }

    /// Compact an oversized loaded index before it reaches the global cache.
    ///
    /// This avoids carrying hundreds of thousands of stale URL keys in memory
    /// when a previous build let the disk cache grow beyond the intended budget.
    pub fn compact_to_size(&mut self, target_size: u64) -> Vec<CacheEntry> {
        if self.total_size_bytes <= target_size {
            return Vec::new();
        }

        let now = current_timestamp();
        let mut scored: Vec<(String, CacheEntry, f64)> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                now.saturating_sub(entry.last_accessed) > DISPLAYED_EMOTE_RETENTION_SECS
            })
            .map(|(url, entry)| (url.clone(), entry.clone(), entry.eviction_score()))
            .collect();
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let mut kept: IndexMap<String, CacheEntry> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                now.saturating_sub(entry.last_accessed) <= DISPLAYED_EMOTE_RETENTION_SECS
            })
            .map(|(url, entry)| (url.clone(), entry.clone()))
            .collect();
        let mut removed = Vec::new();
        let mut kept_bytes = kept
            .values()
            .fold(0u64, |total, entry| total.saturating_add(entry.size_bytes));

        for (url, entry, _) in scored {
            let can_keep = kept_bytes.saturating_add(entry.size_bytes) <= target_size;
            if can_keep {
                kept_bytes = kept_bytes.saturating_add(entry.size_bytes);
                kept.insert(url, entry);
            } else {
                removed.push(entry);
            }
        }

        self.entries = kept;
        self.total_size_bytes = kept_bytes;
        removed
    }
}

impl Default for DiskCacheIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl From<SerializableIndex> for DiskCacheIndex {
    fn from(s: SerializableIndex) -> Self {
        Self {
            version: s.version,
            entries: s.entries.into_iter().collect(),
            total_size_bytes: s.total_size_bytes,
        }
    }
}

impl From<&DiskCacheIndex> for SerializableIndex {
    fn from(index: &DiskCacheIndex) -> Self {
        Self {
            version: index.version,
            entries: index
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            total_size_bytes: index.total_size_bytes,
        }
    }
}

// ============================================================================
/// Get current Unix timestamp in seconds
pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_static(hash: &str, size: u64) -> CacheEntry {
        let mut entry = CacheEntry::new_static(hash.to_string(), size, 28, 28);
        entry.last_accessed = current_timestamp()
            .saturating_sub(DISPLAYED_EMOTE_RETENTION_SECS)
            .saturating_sub(1);
        entry
    }

    #[test]
    fn test_cache_entry_eviction_score() {
        let mut entry = CacheEntry::new_static("hash".to_string(), 1000, 28, 28);
        let score1 = entry.eviction_score();

        entry.access_count = 100;
        let score2 = entry.eviction_score();

        assert!(score2 > score1);
    }

    #[test]
    fn provider_url_variants_and_key_strings_are_stable() {
        let variant = MediaVariant::for_emote_url(
            "7tv",
            "2x",
            "https://cdn.7tv.app/emote/abc/2x.WEBP?frame=1",
            true,
        );
        assert_eq!(variant.format.as_ref(), "webp");
        assert_eq!(variant.animated_kind, MediaAnimatedKind::Animated);
        let id = MediaSourceId::new("7tv", "abc", variant);
        assert_eq!(id.key_string(), "7tv:abc:2x:webp:animated");
    }

    #[test]
    fn source_blob_replacement_preserves_byte_accounting() {
        let mut index = SourceCacheIndex::new();
        assert!(index.insert_blob(SourceBlobEntry::new(
            "hash".to_string(),
            SourceBlobEncoding::Raw,
            10,
            10,
            28,
            28,
        )));
        assert_eq!(index.total_size_bytes, 10);
        assert!(!index.insert_blob(SourceBlobEntry::new(
            "hash".to_string(),
            SourceBlobEncoding::Raw,
            24,
            24,
            28,
            28,
        )));
        assert_eq!(index.total_size_bytes, 24);
    }

    #[test]
    fn alias_removal_and_orphan_cleanup_are_consistent() {
        let mut index = SourceCacheIndex::new();
        index.insert_blob(SourceBlobEntry::new(
            "hash".to_string(),
            SourceBlobEncoding::Raw,
            10,
            10,
            28,
            28,
        ));
        index.insert_aliases(
            Some("identity".to_string()),
            "https://cdn.example/item".to_string(),
            "hash".to_string(),
        );
        assert_eq!(index.blob_alias_count("hash"), 2);
        assert_eq!(
            index.remove_url_alias("https://cdn.example/item"),
            Some("hash".to_string())
        );
        assert_eq!(index.blob_alias_count("hash"), 1);
        assert_eq!(
            index.remove_identity_alias("identity"),
            Some("hash".to_string())
        );
        assert_eq!(index.blob_alias_count("hash"), 0);
        assert_eq!(index.remove_blob("hash").unwrap().size_bytes, 10);
        assert!(index.is_empty());
        assert_eq!(index.total_size_bytes, 0);
    }

    #[test]
    fn retention_eviction_and_compaction_keep_exact_totals() {
        let mut index = DiskCacheIndex::new();
        index.insert("old".to_string(), old_static("old", 10));
        index.insert(
            "fresh".to_string(),
            CacheEntry::new_static("fresh".to_string(), 10, 28, 28),
        );
        assert_eq!(index.get_eviction_candidates(10)[0].0, "old");
        let removed = index.compact_to_size(10);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].hash, "old");
        assert_eq!(index.total_size_bytes, 10);
        assert!(index.get("fresh").is_some());
    }

    #[test]
    fn postcard_roundtrips_preserve_index_schema() {
        let mut disk = DiskCacheIndex::new();
        disk.insert(
            "https://example/item".to_string(),
            CacheEntry::new_static("hash".to_string(), 12, 28, 28),
        );
        let encoded = postcard::to_stdvec(&SerializableIndex::from(&disk)).unwrap();
        let (decoded, remaining): (SerializableIndex, _) =
            postcard::take_from_bytes(&encoded).unwrap();
        assert!(remaining.is_empty());
        let restored = DiskCacheIndex::from(decoded);
        assert_eq!(restored.version, DISK_CACHE_VERSION);
        assert_eq!(restored.total_size_bytes, 12);
        assert_eq!(restored.get("https://example/item").unwrap().hash, "hash");

        let mut source = SourceCacheIndex::new();
        source.insert_blob(SourceBlobEntry::new(
            "blob".to_string(),
            SourceBlobEncoding::Zstd,
            7,
            11,
            28,
            28,
        ));
        source.insert_aliases(
            Some("identity".to_string()),
            "https://example/source".to_string(),
            "blob".to_string(),
        );
        let encoded = postcard::to_stdvec(&SerializableSourceIndex::from(&source)).unwrap();
        let (decoded, remaining): (SerializableSourceIndex, _) =
            postcard::take_from_bytes(&encoded).unwrap();
        assert!(remaining.is_empty());
        let restored = SourceCacheIndex::from(decoded);
        assert_eq!(restored.version, SOURCE_CACHE_VERSION);
        assert_eq!(restored.total_size_bytes, 7);
        assert_eq!(restored.blob_alias_count("blob"), 2);
    }

    #[test]
    fn version_constants_are_stable() {
        assert_eq!(DISK_CACHE_VERSION, 1);
        assert_eq!(SOURCE_CACHE_VERSION, 1);
        assert_eq!(DISPLAYED_EMOTE_RETENTION_SECS, 259_200);
    }
}
