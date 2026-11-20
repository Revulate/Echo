//! Persistent media cache on disk.
//!
//! Two stores share one directory and one byte budget:
//! - the URL index and `images/`: zstd-compressed static images keyed by URL;
//! - the source index and `blobs/`: original provider bytes keyed by media
//!   identity, content-addressed so several URLs or identities can share one
//!   blob.
//!
//! ```text
//! <cache root>/
//! ├── index.postcard.zst          # URL -> CacheEntry
//! ├── images/{hash}.zst           # compressed static image bytes
//! ├── source_index.postcard.zst   # identity and URL aliases -> SourceBlobEntry
//! ├── blobs/{hash[0..2]}/{hash}.* # source bytes, raw or zstd
//! └── frames/{hash}/              # animated entries of older caches; only
//!                                 # evicted and cleaned up, never written
//! ```
//!
//! Sizes are charged to the shared [`BudgetManager`] and counted in
//! [`CacheStats`]; eviction runs when the total passes
//! [`EVICTION_THRESHOLD`] of [`MAX_DISK_CACHE_BYTES`].

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use bincode::Decode;
use dashmap::DashMap;
use tokio::sync::{Mutex, RwLock, watch};

use crate::persist;
use echo_cache_policy::{BudgetManager, CacheStats};

// ============================================================================
// Constants
// ============================================================================

/// Maximum disk cache size (500MB)
pub const MAX_DISK_CACHE_BYTES: u64 = 500 * 1024 * 1024;
/// Maximum compressed source entry size accepted into the disk cache.
pub const MAX_DISK_CACHE_ENTRY_BYTES: usize = 64 * 1024 * 1024;
/// Maximum age for compressed source entries before they are expired on read.
pub const MAX_DISK_CACHE_ENTRY_AGE_SECS: u64 = 14 * 24 * 60 * 60;
/// Eviction threshold (90% of max)
pub const EVICTION_THRESHOLD: f64 = 0.9;

/// Eviction target (70% of max)
pub const EVICTION_TARGET: f64 = 0.7;

/// Current Postcard index file name.
pub const INDEX_FILE_NAME: &str = "index.postcard.zst";
/// Retired Bincode index name, accepted only for one-way recovery.
pub const LEGACY_INDEX_FILE_NAME: &str = "index.bincode.zst";
/// Source blob index file name.
pub const SOURCE_INDEX_FILE_NAME: &str = "source_index.postcard.zst";
/// Retired Bincode source-index name, accepted only for one-way recovery.
pub const LEGACY_SOURCE_INDEX_FILE_NAME: &str = "source_index.bincode.zst";
/// Bound compressed index input before creating a zstd decoder.
const MAX_INDEX_COMPRESSED_BYTES: u64 = 16 * 1024 * 1024;
/// Bound decompressed index input before deserialization.
const MAX_INDEX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// Frame cache subdirectory
pub const FRAMES_DIR: &str = "frames";

/// Image cache subdirectory
pub const IMAGES_DIR: &str = "images";
/// Content-addressed source blob directory.
pub const BLOBS_DIR: &str = "blobs";
/// Persist index after this many inserts even if app is still running.
const INDEX_AUTOSAVE_EVERY_INSERTS: u64 = 200;
/// Persist index at least this often when inserts are ongoing.
const INDEX_AUTOSAVE_MAX_DELAY_SECS: u64 = 30;
/// Minimum wall-clock interval between *count*-triggered index saves. Under chat load the
/// 200-insert threshold is reached several times per second; without this floor the whole
/// ~643KB index is re-serialized (zstd + temp + rename) ~5x/sec. The dirty counter stays
/// elevated while throttled (it only resets on a successful save), so a burst of touches
/// collapses into one write; the 30s time backstop + eviction/shutdown saves bound staleness.
const INDEX_AUTOSAVE_MIN_INTERVAL_SECS: u64 = 5;
/// Minimum compression win before storing a source blob as zstd.
const SOURCE_ZSTD_MIN_SAVINGS_NUMERATOR: usize = 95;
const SOURCE_ZSTD_MIN_SAVINGS_DENOMINATOR: usize = 100;
/// Source blobs are normally CDN image bytes, which are already compressed.
/// Only try zstd for unknown formats to avoid spending startup CPU proving
/// PNG/WebP/GIF/JPEG media cannot be compressed meaningfully.
const SOURCE_ZSTD_UNKNOWN_FORMAT_MAX_BYTES: usize = 256 * 1024;
/// How long after [`DiskLayer::initialize`] starts loading the persisted
/// indexes disk lookups wait for them. The window is shared, so a lookup made
/// later in a slow load, or a second lookup for the same request, does not
/// wait again; one that gives up misses and downloads, as with no cache.
const INDEX_LOAD_LOOKUP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

// ============================================================================
// Types
// ============================================================================

pub use echo_cache_policy::disk_model::{
    CacheEntry, DISK_CACHE_VERSION, DISPLAYED_EMOTE_RETENTION_SECS, DiskCacheIndex, EntryType,
    MediaAnimatedKind, MediaSourceId, MediaVariant, SOURCE_CACHE_VERSION, SourceAlias,
    SourceBlobEncoding, SourceBlobEntry, SourceCacheIndex,
};
use echo_cache_policy::disk_model::{
    SerializableIndex, SerializableSourceIndex, current_timestamp,
};

mod legacy;
use legacy::{LegacySerializableIndex, LegacySerializableSourceIndex};

// ============================================================================
// Disk Layer
// ============================================================================

/// Persistent disk cache layer
pub struct DiskLayer {
    /// Base directory for cache files
    pub base_path: PathBuf,
    /// Cache index
    pub index: Arc<RwLock<DiskCacheIndex>>,
    /// Provider-ID/content-addressed source blob index
    pub source_index: Arc<RwLock<SourceCacheIndex>>,
    /// Current total size in bytes
    pub total_size: AtomicU64,
    /// Whether initialized
    pub initialized: AtomicBool,
    /// While [`Self::initialize`] is loading the persisted indexes, the
    /// instant until which lookups wait for them. Index writes wait until it
    /// clears, so installing the loaded indexes cannot overwrite them.
    index_loading: watch::Sender<Option<tokio::time::Instant>>,
    /// Source aliases currently being saved
    pub saving_sources: DashMap<String, ()>,
    /// True while a disk eviction pass is running (single-flight guard).
    eviction_in_progress: AtomicBool,
    /// Save bookkeeping for the legacy index.
    index_save: Arc<IndexSaveState>,
    /// Save bookkeeping for the source blob index.
    source_index_save: Arc<IndexSaveState>,
    /// Budget manager reference
    _budget: Arc<BudgetManager>,
    /// Statistics
    stats: Arc<CacheStats>,
}

/// Save bookkeeping for one persisted index. It is shared through an `Arc`
/// with the save an autosave spawns, so the write that triggered the save
/// does not wait for it.
struct IndexSaveState {
    /// Serializes saves of this index to avoid temp-file rename races.
    lock: Mutex<()>,
    /// Index writes the last completed save's snapshot did not cover.
    dirty_writes: AtomicU64,
    /// Last time the index was saved (unix seconds).
    last_save_ts: AtomicU64,
}

impl IndexSaveState {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            dirty_writes: AtomicU64::new(0),
            last_save_ts: AtomicU64::new(current_timestamp()),
        }
    }

    /// Count one index write. Returns `(writes, seconds since the last save,
    /// due by count)` when the autosave policy wants a save.
    fn record_write(&self) -> Option<(u64, u64, bool)> {
        // Release: a save that sees this count also sees the index write
        // made before it.
        let writes = self.dirty_writes.fetch_add(1, Ordering::AcqRel) + 1;
        let elapsed = current_timestamp().saturating_sub(self.last_save_ts.load(Ordering::Relaxed));
        let due_by_count = writes >= INDEX_AUTOSAVE_EVERY_INSERTS;
        let due_by_time = elapsed >= INDEX_AUTOSAVE_MAX_DELAY_SECS;
        let count_save_ok = due_by_count && elapsed >= INDEX_AUTOSAVE_MIN_INTERVAL_SECS;
        (count_save_ok || due_by_time).then_some((writes, elapsed, due_by_count))
    }

    /// Writes counted so far; read under [`Self::lock`] before the snapshot.
    fn writes_before_snapshot(&self) -> u64 {
        self.dirty_writes.load(Ordering::Acquire)
    }

    /// Record a completed save whose snapshot was taken after `saved_writes`
    /// writes were counted. Writes counted since stay for the next save.
    fn mark_saved(&self, saved_writes: u64) {
        let _ = self
            .dirty_writes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |writes| {
                Some(writes.saturating_sub(saved_writes))
            });
        self.last_save_ts
            .store(current_timestamp(), Ordering::Relaxed);
    }
}

/// Clears the in-flight `saving_sources` marker on drop so a panic or
/// future-cancellation during the blocking save can't strand the key, which
/// would silently no-op all future saves for that identity.
struct SavingSourceGuard<'a> {
    saving_sources: &'a DashMap<String, ()>,
    key: String,
}

impl Drop for SavingSourceGuard<'_> {
    fn drop(&mut self) {
        self.saving_sources.remove(&self.key);
    }
}

impl DiskLayer {
    /// Saturating subtract for `AtomicU64` to avoid underflow when multiple
    /// cache tasks adjust byte counters concurrently.
    fn atomic_saturating_sub(counter: &AtomicU64, amount: u64) {
        let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(amount))
        });
    }

    fn atomic_usize_saturating_sub(counter: &AtomicUsize, amount: usize) {
        let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(amount))
        });
    }

    /// Create a new disk layer
    pub fn new(base_path: PathBuf, budget: Arc<BudgetManager>, stats: Arc<CacheStats>) -> Self {
        Self {
            base_path,
            index: Arc::new(RwLock::new(DiskCacheIndex::new())),
            source_index: Arc::new(RwLock::new(SourceCacheIndex::new())),
            total_size: AtomicU64::new(0),
            initialized: AtomicBool::new(false),
            index_loading: watch::Sender::new(None),
            saving_sources: DashMap::new(),
            eviction_in_progress: AtomicBool::new(false),
            index_save: Arc::new(IndexSaveState::new()),
            source_index_save: Arc::new(IndexSaveState::new()),
            _budget: budget,
            stats,
        }
    }

    /// Get the frames directory path
    pub fn frames_dir(&self) -> PathBuf {
        self.base_path.join(FRAMES_DIR)
    }

    /// Get the images directory path
    pub fn images_dir(&self) -> PathBuf {
        self.base_path.join(IMAGES_DIR)
    }

    /// Get the source blob directory path.
    pub fn blobs_dir(&self) -> PathBuf {
        self.base_path.join(BLOBS_DIR)
    }

    /// Get the index file path
    pub fn index_path(&self) -> PathBuf {
        self.base_path.join(INDEX_FILE_NAME)
    }

    fn legacy_index_path(&self) -> PathBuf {
        self.base_path.join(LEGACY_INDEX_FILE_NAME)
    }

    /// Get the source blob index path.
    pub fn source_index_path(&self) -> PathBuf {
        self.base_path.join(SOURCE_INDEX_FILE_NAME)
    }

    fn legacy_source_index_path(&self) -> PathBuf {
        self.base_path.join(LEGACY_SOURCE_INDEX_FILE_NAME)
    }

    fn source_blob_path(&self, hash: &str, encoding: SourceBlobEncoding) -> PathBuf {
        source_blob_path_for(&self.blobs_dir(), hash, encoding)
    }

    /// Wait while [`Self::initialize`] is loading the persisted indexes: a
    /// lookup (`until_load_ends == false`) until the shared lookup window
    /// closes, a write until the load ends. Returns at once when no load is in
    /// progress, including when none was started.
    async fn wait_for_index_load(&self, until_load_ends: bool) {
        let Some(lookup_deadline) = *self.index_loading.borrow() else {
            return;
        };
        let mut loading = self.index_loading.subscribe();
        let loaded = loading.wait_for(Option::is_none);
        if until_load_ends {
            let _ = loaded.await;
        } else {
            let _ = tokio::time::timeout_at(lookup_deadline, loaded).await;
        }
    }

    /// Initialize the disk cache
    pub async fn initialize(&self) -> Result<(), std::io::Error> {
        if self.initialized.load(Ordering::Acquire) {
            return Ok(());
        }

        // Clears the loading flag on every exit, including errors and panics,
        // so writers waiting on it never hang.
        struct IndexLoadGuard<'a>(&'a watch::Sender<Option<tokio::time::Instant>>);
        impl Drop for IndexLoadGuard<'_> {
            fn drop(&mut self) {
                self.0.send_replace(None);
            }
        }
        self.index_loading
            .send_replace(Some(tokio::time::Instant::now() + INDEX_LOAD_LOOKUP_WAIT));
        let index_load_guard = IndexLoadGuard(&self.index_loading);

        crate::log_cache!(info, path = %self.base_path.display(), "Initializing disk cache layer");

        // Create directories
        tokio::fs::create_dir_all(&self.base_path).await?;
        tokio::fs::create_dir_all(self.frames_dir()).await?;
        tokio::fs::create_dir_all(self.images_dir()).await?;
        tokio::fs::create_dir_all(self.blobs_dir()).await?;

        // Load existing indexes into local values first so oversized historical
        // caches can be compacted before their maps become long-lived globals.
        let mut legacy_index = DiskCacheIndex::new();
        let index_path = self.index_path();
        let legacy_index_path = self.legacy_index_path();
        // Unlisted legacy files are only swept when the loaded index is what
        // is on disk; after a failed load it may still list them.
        let mut legacy_index_read = true;
        match load_index(&index_path, &legacy_index_path).await {
            Ok(loaded_index) => {
                crate::log_cache!(
                    info,
                    entries = loaded_index.len(),
                    size_mb = loaded_index.total_size_bytes / (1024 * 1024),
                    "Loaded disk cache index"
                );
                legacy_index = loaded_index;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::log_cache!(
                    debug,
                    path = %index_path.display(),
                    legacy_path = %legacy_index_path.display(),
                    "No disk cache index found, starting fresh"
                );
            }
            Err(error) => {
                legacy_index_read = false;
                crate::log_cache!(
                    warn,
                    path = %index_path.display(),
                    legacy_path = %legacy_index_path.display(),
                    error = %error,
                    "Failed to load disk cache index; starting with an empty reconstructible cache"
                );
            }
        }

        let mut source_index = SourceCacheIndex::new();
        let source_index_path = self.source_index_path();
        let legacy_source_index_path = self.legacy_source_index_path();
        match load_source_index(&source_index_path, &legacy_source_index_path).await {
            Ok(loaded_source_index) => {
                crate::log_cache!(
                    info,
                    blobs = loaded_source_index.len(),
                    size_mb = loaded_source_index.total_size_bytes / (1024 * 1024),
                    "Loaded source blob cache index"
                );
                source_index = loaded_source_index;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::log_cache!(
                    debug,
                    path = %source_index_path.display(),
                    legacy_path = %legacy_source_index_path.display(),
                    "No source blob cache index found, starting fresh"
                );
            }
            Err(error) => {
                crate::log_cache!(
                    warn,
                    path = %source_index_path.display(),
                    legacy_path = %legacy_source_index_path.display(),
                    error = %error,
                    "Failed to load source blob cache index; starting with an empty reconstructible cache"
                );
            }
        }

        let loaded_indexes = compact_loaded_indexes_blocking(legacy_index, source_index).await?;
        let LoadedDiskIndexes {
            legacy_index,
            source_index,
            pruned_legacy_entries,
            compaction,
        } = loaded_indexes;

        if let Some(compaction) = compaction {
            // Startup compaction only prunes the legacy index; oversized
            // source blobs are ground down by runtime eviction instead.
            crate::log_cache!(
                warn,
                loaded_mb = compaction.loaded_total_bytes / (1024 * 1024),
                target_mb = compaction.target_bytes / (1024 * 1024),
                legacy_pruned = compaction.legacy_pruned,
                legacy_kept = compaction.legacy_kept,
                source_kept = compaction.source_kept,
                "Compacted oversized legacy disk index during startup"
            );
        }

        let mut listed_static = HashSet::new();
        let mut listed_animated = HashSet::new();
        for entry in legacy_index.entries.values() {
            match entry.entry_type {
                EntryType::Static => listed_static.insert(format!("{}.zst", entry.hash)),
                EntryType::Animated => listed_animated.insert(entry.hash.clone()),
            };
        }

        let legacy_total_size = legacy_index.total_size_bytes;
        let legacy_entry_count = legacy_index.len();
        let source_total_size = source_index.total_size_bytes;
        let source_entry_count = source_index.len();
        let total_size = legacy_total_size.saturating_add(source_total_size);

        *self.index.write().await = legacy_index;
        *self.source_index.write().await = source_index;
        self.total_size.store(total_size, Ordering::Release);
        self.stats
            .disk_entries
            .store(legacy_entry_count + source_entry_count, Ordering::Relaxed);
        self.stats
            .disk_bytes
            .store(total_size as usize, Ordering::Relaxed);

        self.initialized.store(true, Ordering::Release);
        drop(index_load_guard);

        let mut legacy_index_on_disk = legacy_index_read;
        if !pruned_legacy_entries.is_empty() {
            self.stats
                .disk_evictions
                .fetch_add(pruned_legacy_entries.len(), Ordering::Relaxed);
            let index_save_result = self.save_index_result().await;
            match index_save_result {
                Ok(()) => {
                    spawn_pruned_entry_cleanup(
                        self.images_dir(),
                        self.frames_dir(),
                        pruned_legacy_entries,
                    );
                }
                Err(e) => {
                    legacy_index_on_disk = false;
                    crate::log_cache!(
                        warn,
                        error = %e,
                        "Skipping startup disk cache file cleanup: compacted index save failed"
                    );
                }
            }
        }
        if legacy_index_on_disk {
            spawn_unlisted_legacy_file_cleanup(
                self.base_path.clone(),
                self.images_dir(),
                self.frames_dir(),
                listed_static,
                listed_animated,
            );
        }

        Ok(())
    }

    /// Get cache entry for a URL
    pub async fn get_entry(&self, url: &str) -> Option<CacheEntry> {
        if let Some(entry) = self.get_source_entry(None, url).await {
            return Some(entry);
        }
        let index = self.index.read().await;
        index.get(url).cloned()
    }

    /// Get cache entry for an identity-aware source request.
    pub async fn get_source_entry(
        &self,
        identity: Option<&MediaSourceId>,
        url: &str,
    ) -> Option<CacheEntry> {
        self.wait_for_index_load(false).await;
        let identity_key = identity.map(MediaSourceId::key_string);
        let mut source_index = self.source_index.write().await;
        let blob_hash = source_index.get_alias_blob_hash(identity_key.as_deref(), url)?;
        source_index.touch_aliases_for_blob(&blob_hash, identity_key.as_deref(), url);
        source_index
            .blobs
            .get(&blob_hash)
            .map(SourceBlobEntry::as_cache_entry)
    }

    /// Snapshot top static-image URLs by warm-start priority.
    ///
    /// Priority balances access frequency and recency so startup hydration spends
    /// eager decode budget on the most likely-to-be-used emotes first.
    pub async fn snapshot_static_urls_by_priority(&self, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }

        let source_index = Arc::clone(&self.source_index);
        let index = Arc::clone(&self.index);
        match tokio::task::spawn_blocking(move || {
            snapshot_static_urls_by_priority_sync(source_index, index, limit)
        })
        .await
        {
            Ok(urls) => urls,
            Err(e) => {
                crate::log_cache!(
                    warn,
                    error = %e,
                    "Warm-start priority snapshot task failed"
                );
                Vec::new()
            }
        }
    }

    /// Update access time for a URL
    pub async fn touch(&self, url: &str) {
        let touched = {
            let mut index = self.index.write().await;
            index.touch(url)
        };
        if touched {
            self.maybe_autosave_index().await;
        }
    }

    /// Mark a displayed emote/media source as recently used for the three-day
    /// retention window.
    ///
    /// This intentionally uses the existing index access timestamps so the
    /// marker is persisted with the normal cache indexes and eviction can skip
    /// recently displayed entries without a second metadata database.
    pub async fn mark_source_displayed(&self, identity: Option<&MediaSourceId>, url: &str) {
        let identity_key = identity.map(MediaSourceId::key_string);
        let mut touched_source = false;
        {
            let mut source_index = self.source_index.write().await;
            if let Some(blob_hash) = source_index.get_alias_blob_hash(identity_key.as_deref(), url)
            {
                source_index.touch_aliases_for_blob(&blob_hash, identity_key.as_deref(), url);
                touched_source = true;
            }
        }

        if touched_source {
            self.maybe_autosave_source_index().await;
            return;
        }

        let touched_legacy = {
            let mut index = self.index.write().await;
            index.touch(url)
        };
        if touched_legacy {
            self.maybe_autosave_index().await;
        }
    }

    /// Add or update a cache entry
    pub async fn insert(&self, url: String, entry: CacheEntry) {
        self.wait_for_index_load(true).await;
        let size = entry.size_bytes;
        let mut is_new_entry = true;

        {
            let mut index = self.index.write().await;
            if let Some(old) = index.get(&url) {
                Self::atomic_saturating_sub(&self.total_size, old.size_bytes);
                Self::atomic_usize_saturating_sub(&self.stats.disk_bytes, old.size_bytes as usize);
                is_new_entry = false;
            }
            index.insert(url, entry);
        }

        self.total_size.fetch_add(size, Ordering::Relaxed);
        if is_new_entry {
            self.stats.disk_entries.fetch_add(1, Ordering::Relaxed);
        }
        self.stats
            .disk_bytes
            .fetch_add(size as usize, Ordering::Relaxed);

        // Check if eviction needed
        self.maybe_evict().await;
        self.maybe_autosave_index().await;
    }

    async fn insert_source_blob(
        &self,
        identity: Option<&MediaSourceId>,
        url: String,
        blob: SourceBlobEntry,
    ) {
        self.wait_for_index_load(true).await;
        let size = blob.size_bytes;
        let identity_key = identity.map(MediaSourceId::key_string);
        let mut is_new_blob = false;

        {
            let mut source_index = self.source_index.write().await;
            if source_index.blobs.contains_key(&blob.hash) {
                if let Some(existing) = source_index.blobs.get_mut(&blob.hash) {
                    existing.touch();
                }
            } else {
                is_new_blob = true;
                source_index.insert_blob(blob.clone());
            }
            source_index.insert_aliases(identity_key, url, blob.hash);
        }

        if is_new_blob {
            self.total_size.fetch_add(size, Ordering::Relaxed);
            self.stats.disk_entries.fetch_add(1, Ordering::Relaxed);
            self.stats
                .disk_bytes
                .fetch_add(size as usize, Ordering::Relaxed);
        }

        self.maybe_evict().await;
        self.maybe_autosave_source_index().await;
    }

    /// Remove URL and/or identity aliases for a source entry. If the blob no
    /// longer has aliases, remove the blob bytes too.
    pub async fn remove_source_aliases(&self, identity: Option<&MediaSourceId>, url: Option<&str>) {
        self.wait_for_index_load(true).await;
        let identity_key = identity.map(MediaSourceId::key_string);
        let mut candidate_blobs = Vec::with_capacity(2);
        let mut removed_blobs = Vec::new();

        {
            let mut source_index = self.source_index.write().await;
            if let Some(url) = url
                && let Some(blob_hash) = source_index.remove_url_alias(url)
            {
                candidate_blobs.push(blob_hash);
            }
            if let Some(identity_key) = identity_key.as_deref()
                && let Some(blob_hash) = source_index.remove_identity_alias(identity_key)
            {
                candidate_blobs.push(blob_hash);
            }

            candidate_blobs.sort_unstable();
            candidate_blobs.dedup();
            for blob_hash in candidate_blobs {
                if source_index.blob_alias_count(&blob_hash) == 0
                    && let Some(blob) = source_index.remove_blob(&blob_hash)
                {
                    removed_blobs.push(blob);
                }
            }
        }

        for blob in removed_blobs {
            let path = self.source_blob_path(&blob.hash, blob.encoding);
            // The index entry is already gone, so a file that was missing
            // gives its bytes back as well.
            let deleted = removed_or_missing(tokio::fs::remove_file(&path).await);
            // A file that could not be deleted (held open, for example) is
            // still on disk, so its entry goes back into the index without
            // aliases and stays counted until eviction removes it. If the
            // same blob was stored again meanwhile, that insert counted it a
            // second time and this copy's bytes are given back.
            let still_indexed = !deleted && {
                let mut source_index = self.source_index.write().await;
                if source_index.blobs.contains_key(&blob.hash) {
                    false
                } else {
                    // Nothing can reach it any more, so eviction takes it first.
                    let mut unreachable = blob.clone();
                    unreachable.last_accessed = 0;
                    unreachable.access_count = 0;
                    source_index.insert_blob(unreachable);
                    true
                }
            };
            if still_indexed {
                continue;
            }
            Self::atomic_saturating_sub(&self.total_size, blob.size_bytes);
            Self::atomic_usize_saturating_sub(&self.stats.disk_entries, 1);
            Self::atomic_usize_saturating_sub(&self.stats.disk_bytes, blob.size_bytes as usize);
            if deleted {
                self.stats.disk_evictions.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.maybe_autosave_source_index().await;
    }

    /// Get current cache size in bytes
    pub fn size(&self) -> u64 {
        self.total_size.load(Ordering::Relaxed)
    }

    /// Check if eviction is needed and perform it
    pub async fn maybe_evict(&self) {
        struct EvictionGuard<'a> {
            flag: &'a AtomicBool,
        }

        impl Drop for EvictionGuard<'_> {
            fn drop(&mut self) {
                self.flag.store(false, Ordering::Release);
            }
        }

        let current_size = self.size();
        let threshold = (MAX_DISK_CACHE_BYTES as f64 * EVICTION_THRESHOLD) as u64;

        if current_size <= threshold {
            return;
        }

        // Avoid launching overlapping eviction passes under heavy concurrent insert load.
        if self
            .eviction_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            crate::log_cache!(trace, "Skipping disk eviction: already in progress");
            return;
        }
        let _eviction_guard = EvictionGuard {
            flag: &self.eviction_in_progress,
        };

        let target = (MAX_DISK_CACHE_BYTES as f64 * EVICTION_TARGET) as u64;

        crate::log_cache!(
            info,
            current_mb = current_size / (1024 * 1024),
            target_mb = target / (1024 * 1024),
            "Starting disk cache eviction"
        );

        let candidates = match select_eviction_candidates_blocking(
            Arc::clone(&self.index),
            Arc::clone(&self.source_index),
            current_size - target,
        )
        .await
        {
            Ok(candidates) => candidates,
            Err(e) => {
                crate::log_cache!(
                    warn,
                    error = %e,
                    "Disk cache eviction candidate selection failed"
                );
                return;
            }
        };
        let DiskEvictionCandidates {
            legacy_entries_to_evict,
            source_entries_to_evict,
        } = candidates;

        let mut evicted_count = 0;
        let mut evicted_bytes = 0u64;

        for (url, entry) in legacy_entries_to_evict {
            let deleted = match entry.entry_type {
                EntryType::Static => {
                    let path = self.images_dir().join(&entry.hash).with_extension("zst");
                    removed_or_missing(tokio::fs::remove_file(&path).await)
                }
                EntryType::Animated => {
                    let path = self.frames_dir().join(&entry.hash);
                    removed_or_missing(tokio::fs::remove_dir_all(&path).await)
                }
            };

            if deleted {
                evicted_bytes += entry.size_bytes;
                evicted_count += 1;

                let mut index = self.index.write().await;
                index.remove(&url);
            }

            if current_size.saturating_sub(evicted_bytes) <= target {
                break;
            }
        }

        if current_size.saturating_sub(evicted_bytes) > target {
            for (blob_hash, entry) in source_entries_to_evict {
                let path = self.source_blob_path(&entry.hash, entry.encoding);
                let deleted = removed_or_missing(tokio::fs::remove_file(&path).await);

                // The file may be gone because an alias removal got to this
                // blob first; whoever takes the entry out of the index gives
                // its bytes back, so they are returned once.
                if deleted
                    && self
                        .source_index
                        .write()
                        .await
                        .remove_blob(&blob_hash)
                        .is_some()
                {
                    evicted_bytes += entry.size_bytes;
                    evicted_count += 1;
                }

                if current_size.saturating_sub(evicted_bytes) <= target {
                    break;
                }
            }
        }

        Self::atomic_saturating_sub(&self.total_size, evicted_bytes);
        Self::atomic_usize_saturating_sub(&self.stats.disk_entries, evicted_count);
        Self::atomic_usize_saturating_sub(&self.stats.disk_bytes, evicted_bytes as usize);
        self.stats
            .disk_evictions
            .fetch_add(evicted_count, Ordering::Relaxed);
        self.stats.record_eviction(evicted_bytes as usize);

        let remaining = current_size.saturating_sub(evicted_bytes);
        if evicted_bytes == 0 && remaining > MAX_DISK_CACHE_BYTES {
            crate::log_cache!(
                warn,
                current_mb = remaining / (1024 * 1024),
                budget_mb = MAX_DISK_CACHE_BYTES / (1024 * 1024),
                "Disk cache over budget but nothing was evictable"
            );
        } else {
            crate::log_cache!(
                info,
                evicted_count,
                evicted_mb = evicted_bytes / (1024 * 1024),
                "Disk cache eviction complete"
            );
        }

        self.save_index().await;
        self.save_source_index().await;
    }

    pub async fn save_index_result(&self) -> Result<(), std::io::Error> {
        // Saving mid-load would write the still-empty index over the file.
        self.wait_for_index_load(true).await;
        let _save_guard = self.index_save.lock.lock().await;

        let saved_writes = self.index_save.writes_before_snapshot();
        save_index_snapshot(&self.index_path(), Arc::clone(&self.index)).await?;
        self.index_save.mark_saved(saved_writes);
        Ok(())
    }

    /// Save the index to disk
    pub async fn save_index(&self) {
        if let Err(e) = self.save_index_result().await {
            crate::log_cache!(warn, error = %e, "Failed to save disk cache index");
        }
    }

    async fn save_source_index_result(&self) -> Result<(), std::io::Error> {
        self.wait_for_index_load(true).await;
        let _save_guard = self.source_index_save.lock.lock().await;

        let saved_writes = self.source_index_save.writes_before_snapshot();
        save_source_index_snapshot(&self.source_index_path(), Arc::clone(&self.source_index))
            .await?;
        self.source_index_save.mark_saved(saved_writes);
        Ok(())
    }

    /// Save the source blob index to disk.
    pub async fn save_source_index(&self) {
        if let Err(e) = self.save_source_index_result().await {
            crate::log_cache!(warn, error = %e, "Failed to save source blob index");
        }
    }

    async fn maybe_autosave_index(&self) {
        let Some((writes, elapsed, due_by_count)) = self.index_save.record_write() else {
            return;
        };
        crate::log_cache!(
            debug,
            writes,
            elapsed_since_last_save_secs = elapsed,
            reason = if due_by_count { "count" } else { "time" },
            "Autosaving disk cache index"
        );
        self.spawn_autosave(&self.index_save, "Failed to save disk cache index", {
            let path = self.index_path();
            let index = Arc::clone(&self.index);
            async move { save_index_snapshot(&path, index).await }
        });
    }

    async fn maybe_autosave_source_index(&self) {
        let Some((writes, elapsed, due_by_count)) = self.source_index_save.record_write() else {
            return;
        };
        crate::log_cache!(
            debug,
            writes,
            elapsed_since_last_save_secs = elapsed,
            reason = if due_by_count { "count" } else { "time" },
            "Autosaving source blob cache index"
        );
        self.spawn_autosave(
            &self.source_index_save,
            "Failed to save source blob index",
            {
                let path = self.source_index_path();
                let index = Arc::clone(&self.source_index);
                async move { save_source_index_snapshot(&path, index).await }
            },
        );
    }

    /// Run an autosave without making the write that triggered it wait.
    ///
    /// Like [`Self::save_index_result`] it first waits for an index load to
    /// end, so it never writes the index the load is about to replace. It is
    /// skipped while another save of the same index holds the lock: during a
    /// burst of inserts that save, or a later one, covers the writes made
    /// meanwhile, instead of one full serialize, compress and fsync per
    /// insert. Writes its snapshot missed stay counted.
    fn spawn_autosave(
        &self,
        state: &Arc<IndexSaveState>,
        failure: &'static str,
        save: impl std::future::Future<Output = Result<(), std::io::Error>> + Send + 'static,
    ) {
        let state = Arc::clone(state);
        let mut loading = self.index_loading.subscribe();
        tokio::spawn(async move {
            let _ = loading.wait_for(Option::is_none).await;
            let Ok(_save_guard) = state.lock.try_lock() else {
                crate::log_cache!(trace, "Skipping index autosave: a save is running");
                return;
            };
            let saved_writes = state.writes_before_snapshot();
            if let Err(e) = save.await {
                crate::log_cache!(warn, error = %e, "{failure}");
                return;
            }
            state.mark_saved(saved_writes);
        });
    }

    // ========================================================================
    // Static Image Operations
    // ========================================================================

    /// Save a static image to disk
    pub async fn save_image(
        &self,
        url: &str,
        bytes: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), std::io::Error> {
        self.save_source_image(None, url, bytes, width, height)
            .await
    }

    /// Save original provider/CDN bytes into the identity-aware source cache.
    pub async fn save_source_image(
        &self,
        identity: Option<&MediaSourceId>,
        url: &str,
        bytes: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), std::io::Error> {
        self.save_source_image_with(identity, url, bytes.len(), || bytes.to_vec(), width, height)
            .await
    }

    /// Save a refcounted download body without copying its complete payload.
    pub async fn save_source_image_owned<B>(
        &self,
        identity: Option<&MediaSourceId>,
        url: &str,
        bytes: B,
        width: u32,
        height: u32,
    ) -> Result<(), std::io::Error>
    where
        B: AsRef<[u8]> + Send + 'static,
    {
        let bytes_len = bytes.as_ref().len();
        self.save_source_image_with(identity, url, bytes_len, || bytes, width, height)
            .await
    }

    async fn save_source_image_with<F, B>(
        &self,
        identity: Option<&MediaSourceId>,
        url: &str,
        bytes_len: usize,
        into_bytes: F,
        width: u32,
        height: u32,
    ) -> Result<(), std::io::Error>
    where
        F: FnOnce() -> B,
        B: AsRef<[u8]> + Send + 'static,
    {
        let identity_label = identity
            .map(MediaSourceId::key_string)
            .unwrap_or_else(|| "none".to_string());
        if bytes_len > MAX_DISK_CACHE_ENTRY_BYTES {
            crate::log_cache!(
                debug,
                url = %url,
                identity = %identity_label,
                size_mb = bytes_len / (1024 * 1024),
                max_mb = MAX_DISK_CACHE_ENTRY_BYTES / (1024 * 1024),
                "Skipping oversized disk cache source entry"
            );
            return Ok(());
        }

        let url_owned = url.to_string();
        let identity_key = if identity_label == "none" {
            None
        } else {
            Some(identity_label.clone())
        };
        let saving_key = identity_key
            .as_deref()
            .map(|key| format!("id:{key}"))
            .unwrap_or_else(|| format!("url:{url}"));

        match self.saving_sources.entry(saving_key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => return Ok(()),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(());
            }
        }
        // RAII: clears the in-flight marker on every exit (incl. panic /
        // future-cancellation across the await) so a stranded key can't
        // silently no-op all future saves for this identity.
        let _saving_guard = SavingSourceGuard {
            saving_sources: &self.saving_sources,
            key: saving_key,
        };

        // Transfer the source buffer only after the in-flight identity gate.
        // Concurrent duplicate saves otherwise copied the complete image bytes
        // before discovering that another task already owned persistence.
        let bytes = into_bytes();
        let blobs_dir = self.blobs_dir();

        let save_result =
            tokio::task::spawn_blocking(move || save_source_blob_sync(&blobs_dir, bytes.as_ref()))
                .await
                .map_err(std::io::Error::other)
                .and_then(|result| result);
        let (hash, encoding, size_bytes, original_size_bytes) = save_result?;

        let entry = SourceBlobEntry::new(
            hash,
            encoding,
            size_bytes,
            original_size_bytes,
            width,
            height,
        );
        self.insert_source_blob(identity, url_owned.clone(), entry)
            .await;

        crate::log_cache!(
            debug,
            url = %url_owned,
            identity = identity_key.as_deref().unwrap_or("none"),
            size_kb = size_bytes / 1024,
            raw_kb = original_size_bytes / 1024,
            encoding = ?encoding,
            "Saved source image to disk cache"
        );
        Ok(())
    }

    /// Load a static image from disk
    pub async fn load_image(&self, url: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = self.load_source_image(None, url).await {
            return Some(bytes);
        }

        let entry = {
            let index = self.index.read().await;
            index.get(url).cloned()
        }?;
        if entry.entry_type != EntryType::Static {
            return None;
        }
        if is_entry_expired(&entry) {
            self.remove_expired_static_entry(url, &entry).await;
            return None;
        }

        self.touch(url).await;
        self.stats.record_hit();

        let image_path = self.images_dir().join(format!("{}.zst", entry.hash));

        tokio::task::spawn_blocking(move || load_image_sync(&image_path))
            .await
            .ok()
            .flatten()
    }

    /// Load original provider/CDN bytes from the identity-aware source cache.
    pub async fn load_source_image(
        &self,
        identity: Option<&MediaSourceId>,
        url: &str,
    ) -> Option<Vec<u8>> {
        self.wait_for_index_load(false).await;
        let identity_key = identity.map(MediaSourceId::key_string);
        let entry = {
            let mut source_index = self.source_index.write().await;
            let blob_hash = source_index.get_alias_blob_hash(identity_key.as_deref(), url)?;
            source_index.touch_aliases_for_blob(&blob_hash, identity_key.as_deref(), url);
            source_index.blobs.get(&blob_hash).cloned()
        }?;

        self.stats.record_hit();
        let image_path = self.source_blob_path(&entry.hash, entry.encoding);

        tokio::task::spawn_blocking(move || load_source_blob_sync(&image_path, entry.encoding))
            .await
            .ok()
            .flatten()
    }

    async fn remove_expired_static_entry(&self, url: &str, entry: &CacheEntry) {
        let image_path = self.images_dir().join(format!("{}.zst", entry.hash));
        let _ = tokio::fs::remove_file(&image_path).await;

        let removed = {
            let mut index = self.index.write().await;
            index.remove(url)
        };

        if let Some(removed) = removed {
            Self::atomic_saturating_sub(&self.total_size, removed.size_bytes);
            Self::atomic_usize_saturating_sub(&self.stats.disk_entries, 1);
            Self::atomic_usize_saturating_sub(&self.stats.disk_bytes, removed.size_bytes as usize);
            self.stats.disk_evictions.fetch_add(1, Ordering::Relaxed);
            crate::log_cache!(
                debug,
                url = %url,
                age_secs = current_timestamp().saturating_sub(removed.last_accessed),
                "Expired disk cache source entry"
            );
            self.maybe_autosave_index().await;
        }
    }
}

// ============================================================================
// Sync Implementations
// ============================================================================

/// Load image synchronously
fn load_image_sync(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = zstd::Decoder::new(file).ok()?;
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes).ok()?;

    crate::log_cache!(trace, path = %path.display(), size_kb = bytes.len() / 1024, "Loaded image from disk");
    Some(bytes)
}

fn save_source_blob_sync(
    blobs_dir: &Path,
    bytes: &[u8],
) -> Result<(String, SourceBlobEncoding, u64, u64), std::io::Error> {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let compressed = if should_try_zstd_for_source_blob(bytes) {
        Some(zstd::stream::encode_all(bytes, 3)?)
    } else {
        None
    };
    let use_compressed = compressed.as_ref().is_some_and(|compressed| {
        compressed
            .len()
            .saturating_mul(SOURCE_ZSTD_MIN_SAVINGS_DENOMINATOR)
            < bytes
                .len()
                .saturating_mul(SOURCE_ZSTD_MIN_SAVINGS_NUMERATOR)
    });
    let (encoding, payload): (SourceBlobEncoding, &[u8]) = if use_compressed {
        (
            SourceBlobEncoding::Zstd,
            compressed.as_ref().expect("checked above").as_slice(),
        )
    } else {
        (SourceBlobEncoding::Raw, bytes)
    };

    let path = source_blob_path_for(blobs_dir, &hash, encoding);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // The name is the content hash, so an existing file should already hold
    // this payload. One that does not (truncated or zeroed by a crash before
    // its data reached the disk) is replaced; trusting it would leave every
    // later download of this blob unable to repair the cache.
    let holds_payload = std::fs::metadata(&path)
        .is_ok_and(|metadata| metadata.len() == payload.len() as u64)
        && std::fs::read(&path).is_ok_and(|existing| existing == payload);
    if holds_payload {
        return Ok((hash, encoding, payload.len() as u64, bytes.len() as u64));
    }

    let temp_path = persist::temp_path_for(&path);
    let written = (|| {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(payload)?;
        file.flush()?;
        drop(file);
        std::fs::rename(&temp_path, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    written?;

    let size = std::fs::metadata(&path)?.len();
    crate::log_cache!(
        trace,
        path = %path.display(),
        size_kb = size / 1024,
        raw_kb = bytes.len() / 1024,
        encoding = ?encoding,
        "Saved source blob"
    );
    Ok((hash, encoding, size, bytes.len() as u64))
}

fn should_try_zstd_for_source_blob(bytes: &[u8]) -> bool {
    bytes.len() <= SOURCE_ZSTD_UNKNOWN_FORMAT_MAX_BYTES && !looks_like_compressed_image(bytes)
}

fn looks_like_compressed_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && &bytes[8..12] == b"avif")
}

fn load_source_blob_sync(path: &Path, encoding: SourceBlobEncoding) -> Option<Vec<u8>> {
    match encoding {
        SourceBlobEncoding::Raw => std::fs::read(path).ok(),
        SourceBlobEncoding::Zstd => {
            let file = std::fs::File::open(path).ok()?;
            let mut decoder = zstd::Decoder::new(file).ok()?;
            let mut bytes = Vec::new();
            decoder.read_to_end(&mut bytes).ok()?;
            Some(bytes)
        }
    }
}

// ============================================================================
// Index Serialization
// ============================================================================

/// Wire configuration used only to read the retired Bincode 2 files. The limit
/// bounds container pre-allocation claimed by a corrupt length prefix.
const LEGACY_BINCODE_CONFIG: bincode::config::Configuration<
    bincode::config::LittleEndian,
    bincode::config::Varint,
    bincode::config::Limit<{ MAX_INDEX_DECOMPRESSED_BYTES }>,
> = bincode::config::standard()
    .with_little_endian()
    .with_variable_int_encoding()
    .with_limit::<{ MAX_INDEX_DECOMPRESSED_BYTES }>();

/// Load the current Postcard index, using the retired Bincode file only when
/// the current file is absent. A corrupt current file is never silently
/// replaced by stale legacy state.
async fn load_index(
    current_path: &Path,
    legacy_path: &Path,
) -> Result<DiskCacheIndex, std::io::Error> {
    let current_path = current_path.to_path_buf();
    let legacy_path = legacy_path.to_path_buf();

    tokio::task::spawn_blocking(move || match load_postcard_index_sync(&current_path) {
        Ok(index) => Ok(index),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let index = load_legacy_index_sync(&legacy_path)?;
            if let Err(error) = save_index_bytes_sync(
                &current_path,
                &encode_index_bytes(SerializableIndex::from(&index))?,
            ) {
                crate::log_cache!(
                    warn,
                    path = %current_path.display(),
                    error = %error,
                    "Loaded legacy disk cache index but could not publish Postcard migration"
                );
            }
            crate::log_cache!(
                info,
                path = %legacy_path.display(),
                "Recovered disk cache index from retired Bincode format"
            );
            Ok(index)
        }
        Err(error) => Err(error),
    })
    .await
    .map_err(std::io::Error::other)?
}

async fn load_source_index(
    current_path: &Path,
    legacy_path: &Path,
) -> Result<SourceCacheIndex, std::io::Error> {
    let current_path = current_path.to_path_buf();
    let legacy_path = legacy_path.to_path_buf();

    tokio::task::spawn_blocking(
        move || match load_postcard_source_index_sync(&current_path) {
            Ok(index) => Ok(index),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let index = load_legacy_source_index_sync(&legacy_path)?;
                if let Err(error) = save_index_bytes_sync(
                    &current_path,
                    &encode_source_index_bytes(SerializableSourceIndex::from(&index))?,
                ) {
                    crate::log_cache!(
                        warn,
                        path = %current_path.display(),
                        error = %error,
                        "Loaded legacy source cache index but could not publish Postcard migration"
                    );
                }
                crate::log_cache!(
                    info,
                    path = %legacy_path.display(),
                    "Recovered source cache index from retired Bincode format"
                );
                Ok(index)
            }
            Err(error) => Err(error),
        },
    )
    .await
    .map_err(std::io::Error::other)?
}

fn load_postcard_index_sync(path: &Path) -> Result<DiskCacheIndex, std::io::Error> {
    let data = read_zstd_limited(path)?;
    let serializable = decode_postcard_exact::<SerializableIndex>(&data, "cache index")?;
    validate_disk_index_version(serializable.version)?;
    let index: DiskCacheIndex = serializable.into();
    crate::log_cache!(debug, entries = index.len(), "Loaded disk cache index");
    Ok(index)
}

fn load_legacy_index_sync(path: &Path) -> Result<DiskCacheIndex, std::io::Error> {
    let data = read_zstd_limited(path)?;
    let serializable: SerializableIndex =
        decode_legacy_bincode_exact::<LegacySerializableIndex>(&data, "cache index")?.into();
    validate_disk_index_version(serializable.version)?;
    Ok(serializable.into())
}

fn load_postcard_source_index_sync(path: &Path) -> Result<SourceCacheIndex, std::io::Error> {
    let data = read_zstd_limited(path)?;
    let serializable =
        decode_postcard_exact::<SerializableSourceIndex>(&data, "source cache index")?;
    validate_source_index_version(serializable.version)?;
    let index: SourceCacheIndex = serializable.into();
    crate::log_cache!(
        debug,
        blobs = index.len(),
        urls = index.url_aliases.len(),
        identities = index.identity_aliases.len(),
        "Loaded source blob cache index"
    );
    Ok(index)
}

fn load_legacy_source_index_sync(path: &Path) -> Result<SourceCacheIndex, std::io::Error> {
    let data = read_zstd_limited(path)?;
    let serializable: SerializableSourceIndex =
        decode_legacy_bincode_exact::<LegacySerializableSourceIndex>(&data, "source cache index")?
            .into();
    validate_source_index_version(serializable.version)?;
    Ok(serializable.into())
}

fn read_zstd_limited(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    persist::read_zstd_bounded(
        path,
        MAX_INDEX_COMPRESSED_BYTES as usize,
        MAX_INDEX_DECOMPRESSED_BYTES,
        "cache index",
    )
}

fn decode_postcard_exact<T>(data: &[u8], label: &str) -> Result<T, std::io::Error>
where
    T: serde::de::DeserializeOwned,
{
    let (value, remaining) = postcard::take_from_bytes(data).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} Postcard decode error: {error}"),
        )
    })?;
    if !remaining.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} Postcard payload has trailing bytes"),
        ));
    }
    Ok(value)
}

fn decode_legacy_bincode_exact<T>(data: &[u8], label: &str) -> Result<T, std::io::Error>
where
    T: Decode<()>,
{
    let (value, consumed) =
        bincode::decode_from_slice(data, LEGACY_BINCODE_CONFIG).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{label} legacy Bincode decode error: {error}"),
            )
        })?;
    if consumed != data.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} legacy Bincode payload has trailing bytes"),
        ));
    }
    Ok(value)
}

fn validate_disk_index_version(version: u32) -> Result<(), std::io::Error> {
    if version == DISK_CACHE_VERSION {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("cache version mismatch: expected {DISK_CACHE_VERSION}, got {version}"),
        ))
    }
}

fn validate_source_index_version(version: u32) -> Result<(), std::io::Error> {
    if version == SOURCE_CACHE_VERSION {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "source cache version mismatch: expected {SOURCE_CACHE_VERSION}, got {version}"
            ),
        ))
    }
}

/// Age after which an index temp file is treated as left by an interrupted
/// save rather than one in progress.
const STALE_INDEX_TEMP_SECS: u64 = 60 * 60;

/// Index name used by the wincode format, which no build reads any more.
const RETIRED_WINCODE_INDEX_FILE_NAME: &str = "index.wincode.zst";

/// Delete files in the retired `images`/`frames` folders that the legacy index
/// does not list, plus index temp files left by interrupted saves and the
/// retired `index.wincode.zst`.
///
/// Sources have been saved to `blobs` since the identity-addressed source
/// cache, so nothing writes to the legacy folders and eviction only removes
/// what the index lists. Files the index lost track of stayed forever; one
/// user cache held 8.4 GB of them. Runs on its own background thread; after
/// the first sweep the folders hold only listed entries.
fn spawn_unlisted_legacy_file_cleanup(
    base_path: PathBuf,
    images_dir: PathBuf,
    frames_dir: PathBuf,
    listed_static: HashSet<String>,
    listed_animated: HashSet<String>,
) {
    let spawn_result = std::thread::Builder::new()
        .name("disk-cache-orphans".into())
        .spawn(move || {
            let removed = remove_unlisted_legacy_files(
                &base_path,
                &images_dir,
                &frames_dir,
                &listed_static,
                &listed_animated,
            );
            if removed.deleted > 0 || removed.failed > 0 {
                crate::log_cache!(
                    info,
                    deleted = removed.deleted,
                    deleted_mb = removed.bytes / (1024 * 1024),
                    failed = removed.failed,
                    "Removed disk cache files no index lists"
                );
            }
        });
    if let Err(e) = spawn_result {
        crate::log_cache!(
            warn,
            error = %e,
            "Failed to start cleanup of unlisted disk cache files"
        );
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RemovedFiles {
    deleted: usize,
    bytes: u64,
    failed: usize,
}

fn remove_unlisted_legacy_files(
    base_path: &Path,
    images_dir: &Path,
    frames_dir: &Path,
    listed_static: &HashSet<String>,
    listed_animated: &HashSet<String>,
) -> RemovedFiles {
    let mut removed = RemovedFiles::default();
    for (dir, listed) in [(images_dir, listed_static), (frames_dir, listed_animated)] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| listed.contains(name)) {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let result = if file_type.is_dir() {
                let bytes = directory_file_bytes(&path);
                std::fs::remove_dir_all(&path).map(|()| bytes)
            } else {
                let bytes = entry.metadata().map_or(0, |metadata| metadata.len());
                std::fs::remove_file(&path).map(|()| bytes)
            };
            match result {
                Ok(bytes) => {
                    removed.deleted += 1;
                    removed.bytes += bytes;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => removed.failed += 1,
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(base_path) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let stale_temp = name.contains(".zst.tmp-")
                && entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age.as_secs() >= STALE_INDEX_TEMP_SECS);
            if (stale_temp || name == RETIRED_WINCODE_INDEX_FILE_NAME)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed.deleted += 1;
            }
        }
    }
    removed
}

/// A removal that found nothing to remove still frees the index entry.
fn removed_or_missing(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn directory_file_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => directory_file_bytes(&entry.path()),
            Ok(_) => entry.metadata().map_or(0, |metadata| metadata.len()),
            Err(_) => 0,
        })
        .sum()
}

fn spawn_pruned_entry_cleanup(
    images_dir: PathBuf,
    frames_dir: PathBuf,
    legacy_entries: Vec<CacheEntry>,
) {
    let legacy_count = legacy_entries.len();
    if legacy_count == 0 {
        return;
    }

    let spawn_result = std::thread::Builder::new()
        .name("disk-cache-prune".into())
        .spawn(move || {
            let mut seen_paths = std::collections::HashSet::new();
            let mut deleted = 0usize;
            let mut missing = 0usize;
            let mut failed = 0usize;

            for entry in legacy_entries {
                let path = match entry.entry_type {
                    EntryType::Static => images_dir.join(&entry.hash).with_extension("zst"),
                    EntryType::Animated => frames_dir.join(&entry.hash),
                };
                if !seen_paths.insert(path.clone()) {
                    continue;
                }

                let result = match entry.entry_type {
                    EntryType::Static => std::fs::remove_file(&path),
                    EntryType::Animated => std::fs::remove_dir_all(&path),
                };
                match result {
                    Ok(()) => deleted += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing += 1,
                    Err(_) => failed += 1,
                }
            }

            crate::log_cache!(
                info,
                legacy_count,
                deleted,
                missing,
                failed,
                "Background cleanup of pruned legacy disk cache files complete"
            );
        });

    match spawn_result {
        Ok(_handle) => {
            crate::log_cache!(
                info,
                legacy_count,
                "Background cleanup of pruned legacy disk cache files started"
            );
        }
        Err(e) => {
            crate::log_cache!(
                warn,
                legacy_count,
                error = %e,
                "Failed to start background cleanup of pruned disk cache files"
            );
        }
    }
}

struct LoadedDiskIndexes {
    legacy_index: DiskCacheIndex,
    source_index: SourceCacheIndex,
    pruned_legacy_entries: Vec<CacheEntry>,
    compaction: Option<StartupIndexCompaction>,
}

struct StartupIndexCompaction {
    loaded_total_bytes: u64,
    target_bytes: u64,
    legacy_pruned: usize,
    legacy_kept: usize,
    source_kept: usize,
}

struct DiskEvictionCandidates {
    legacy_entries_to_evict: Vec<(String, CacheEntry)>,
    source_entries_to_evict: Vec<(String, SourceBlobEntry)>,
}

async fn compact_loaded_indexes_blocking(
    legacy_index: DiskCacheIndex,
    source_index: SourceCacheIndex,
) -> Result<LoadedDiskIndexes, std::io::Error> {
    tokio::task::spawn_blocking(move || compact_loaded_indexes(legacy_index, source_index))
        .await
        .map_err(std::io::Error::other)
}

fn compact_loaded_indexes(
    mut legacy_index: DiskCacheIndex,
    source_index: SourceCacheIndex,
) -> LoadedDiskIndexes {
    let mut pruned_legacy_entries = Vec::new();
    let loaded_total_bytes = legacy_index
        .total_size_bytes
        .saturating_add(source_index.total_size_bytes);
    let threshold = (MAX_DISK_CACHE_BYTES as f64 * EVICTION_THRESHOLD) as u64;
    let target_bytes = (MAX_DISK_CACHE_BYTES as f64 * EVICTION_TARGET) as u64;
    let mut compaction = None;

    if loaded_total_bytes > threshold {
        let legacy_target = target_bytes.saturating_sub(source_index.total_size_bytes);
        pruned_legacy_entries = legacy_index.compact_to_size(legacy_target);
        compaction = Some(StartupIndexCompaction {
            loaded_total_bytes,
            target_bytes,
            legacy_pruned: pruned_legacy_entries.len(),
            legacy_kept: legacy_index.len(),
            source_kept: source_index.len(),
        });
    }

    LoadedDiskIndexes {
        legacy_index,
        source_index,
        pruned_legacy_entries,
        compaction,
    }
}

async fn select_eviction_candidates_blocking(
    index: Arc<RwLock<DiskCacheIndex>>,
    source_index: Arc<RwLock<SourceCacheIndex>>,
    bytes_needed: u64,
) -> Result<DiskEvictionCandidates, std::io::Error> {
    tokio::task::spawn_blocking(move || {
        let legacy_entries_to_evict = {
            let index = index.blocking_read();
            index.get_eviction_candidates(bytes_needed)
        };
        let source_entries_to_evict = {
            let source_index = source_index.blocking_read();
            source_index.get_eviction_candidates(bytes_needed)
        };

        DiskEvictionCandidates {
            legacy_entries_to_evict,
            source_entries_to_evict,
        }
    })
    .await
    .map_err(std::io::Error::other)
}

fn snapshot_static_urls_by_priority_sync(
    source_index: Arc<RwLock<SourceCacheIndex>>,
    index: Arc<RwLock<DiskCacheIndex>>,
    limit: usize,
) -> Vec<String> {
    let now = current_timestamp();
    let mut scored: Vec<(String, f64)> = {
        let source_index = source_index.blocking_read();
        source_index
            .url_aliases
            .iter()
            .filter_map(|(url, alias)| {
                let blob = source_index.blobs.get(&alias.blob_hash)?;
                let access_score = (blob.access_count as f64 + 1.0).ln_1p();
                let age_secs = now.saturating_sub(blob.last_accessed) as f64;
                let recency_score = 1.0 / (1.0 + age_secs / (6.0 * 3600.0));
                let score = access_score * 1.35 + recency_score * 1.9;
                Some((url.clone(), score))
            })
            .collect()
    };

    {
        let index = index.blocking_read();
        scored.extend(index.entries.iter().filter_map(|(url, entry)| {
            if entry.entry_type != EntryType::Static {
                return None;
            }

            // Frequency favors hot emotes, recency keeps current-session channels warm.
            let access_score = (entry.access_count as f64 + 1.0).ln_1p();
            let age_secs = now.saturating_sub(entry.last_accessed) as f64;
            let recency_score = 1.0 / (1.0 + age_secs / (6.0 * 3600.0));
            let score = access_score * 1.35 + recency_score * 1.9;
            Some((url.clone(), score))
        }));
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored.into_iter().map(|(url, _)| url).collect()
}

/// Encode a snapshot of the disk index to a compact binary payload.
fn encode_index_bytes(serializable: SerializableIndex) -> Result<Vec<u8>, std::io::Error> {
    postcard::to_stdvec(&serializable).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("cache index Postcard encode error: {error}"),
        )
    })
}

fn encode_source_index_bytes(
    serializable: SerializableSourceIndex,
) -> Result<Vec<u8>, std::io::Error> {
    postcard::to_stdvec(&serializable).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("source cache index Postcard encode error: {error}"),
        )
    })
}

async fn save_index_snapshot(
    path: &Path,
    index: Arc<RwLock<DiskCacheIndex>>,
) -> Result<(), std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let serializable = {
            let index = index.blocking_read();
            SerializableIndex::from(&*index)
        };
        let data = encode_index_bytes(serializable)?;
        save_index_bytes_sync(&path, &data)
    })
    .await
    .map_err(std::io::Error::other)?
}

async fn save_source_index_snapshot(
    path: &Path,
    index: Arc<RwLock<SourceCacheIndex>>,
) -> Result<(), std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let serializable = {
            let index = index.blocking_read();
            SerializableSourceIndex::from(&*index)
        };
        let data = encode_source_index_bytes(serializable)?;
        save_index_bytes_sync(&path, &data)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Save pre-encoded index bytes to disk.
fn save_index_bytes_sync(path: &Path, data: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    persist::write_atomically(path, |file| {
        let mut encoder = zstd::Encoder::new(file, 6)?;
        encoder.write_all(data)?;
        encoder.finish().map(drop)
    })?;

    crate::log_cache!(debug, path = %path.display(), "Saved disk cache index");
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// True when a source cache entry is old enough to be pruned on demand.
fn is_entry_expired(entry: &CacheEntry) -> bool {
    is_entry_expired_at(entry, current_timestamp())
}

fn is_entry_expired_at(entry: &CacheEntry, now: u64) -> bool {
    now.saturating_sub(entry.last_accessed) > MAX_DISK_CACHE_ENTRY_AGE_SECS
}

fn source_blob_path_for(blobs_dir: &Path, hash: &str, encoding: SourceBlobEncoding) -> PathBuf {
    let prefix = hash.get(0..2).unwrap_or("00");
    blobs_dir
        .join(prefix)
        .join(format!("{}.{}", hash, encoding.extension()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expired_entry_detection() {
        let now = 2_000_000_000;
        let mut entry = CacheEntry::new_static("hash".to_string(), 1000, 28, 28);
        entry.last_accessed = now;
        assert!(!is_entry_expired_at(&entry, now));

        entry.last_accessed = now
            .saturating_sub(MAX_DISK_CACHE_ENTRY_AGE_SECS)
            .saturating_sub(1);
        assert!(is_entry_expired_at(&entry, now));
    }

    #[test]
    fn saving_a_source_blob_replaces_a_truncated_file() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"GIF89a source bytes that stay uncompressed";
        let (hash, encoding, size, _) = save_source_blob_sync(temp.path(), bytes).unwrap();
        let path = source_blob_path_for(temp.path(), &hash, encoding);
        let stored = std::fs::read(&path).unwrap();
        assert_eq!(size, stored.len() as u64);

        std::fs::write(&path, &stored[..stored.len() / 2]).unwrap();
        save_source_blob_sync(temp.path(), bytes).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), stored);
    }

    #[test]
    fn unlisted_legacy_files_are_removed_and_listed_ones_kept() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let images = base.join(IMAGES_DIR);
        let frames = base.join(FRAMES_DIR);
        std::fs::create_dir_all(frames.join("listed-anim")).unwrap();
        std::fs::create_dir_all(frames.join("orphan-anim")).unwrap();
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("listed.zst"), [1u8; 4]).unwrap();
        std::fs::write(images.join("orphan.zst"), [1u8; 8]).unwrap();
        std::fs::write(frames.join("orphan-anim").join("0.zst"), [1u8; 16]).unwrap();
        std::fs::write(base.join(RETIRED_WINCODE_INDEX_FILE_NAME), [0u8]).unwrap();
        let fresh_temp = base.join("index.postcard.zst.tmp-1-1");
        std::fs::write(&fresh_temp, [0u8]).unwrap();
        std::fs::write(base.join(INDEX_FILE_NAME), [0u8]).unwrap();

        let removed = remove_unlisted_legacy_files(
            base,
            &images,
            &frames,
            &HashSet::from(["listed.zst".to_string()]),
            &HashSet::from(["listed-anim".to_string()]),
        );

        assert_eq!(removed.deleted, 3);
        assert_eq!(removed.bytes, 24);
        assert!(images.join("listed.zst").exists());
        assert!(!images.join("orphan.zst").exists());
        assert!(frames.join("listed-anim").exists());
        assert!(!frames.join("orphan-anim").exists());
        assert!(!base.join(RETIRED_WINCODE_INDEX_FILE_NAME).exists());
        // A temp file this new may belong to a save in progress.
        assert!(fresh_temp.exists());
        assert!(base.join(INDEX_FILE_NAME).exists());
    }

    #[test]
    fn postcard_index_payload_rejects_trailing_bytes() {
        let encoded = encode_index_bytes(SerializableIndex::from(&DiskCacheIndex::new())).unwrap();
        let mut with_trailing_byte = encoded;
        with_trailing_byte.push(0);

        assert!(decode_postcard_exact::<SerializableIndex>(&with_trailing_byte, "test").is_err());
    }

    #[tokio::test]
    async fn legacy_index_is_used_only_when_current_index_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        let current_path = temp.path().join(INDEX_FILE_NAME);
        let legacy_path = temp.path().join(LEGACY_INDEX_FILE_NAME);
        let mut index = DiskCacheIndex::new();
        index.insert(
            "https://example.test/legacy.png".to_string(),
            CacheEntry::new_static("legacy".to_string(), 12, 28, 28),
        );
        let encoded = bincode::encode_to_vec(
            LegacySerializableIndex::from(SerializableIndex::from(&index)),
            LEGACY_BINCODE_CONFIG,
        )
        .unwrap();
        std::fs::write(
            &legacy_path,
            zstd::encode_all(encoded.as_slice(), 3).unwrap(),
        )
        .unwrap();

        let loaded = load_index(&current_path, &legacy_path).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(current_path.exists());

        std::fs::write(
            &current_path,
            zstd::encode_all(&b"not a postcard"[..], 3).unwrap(),
        )
        .unwrap();
        assert!(load_index(&current_path, &legacy_path).await.is_err());
    }

    #[tokio::test]
    async fn legacy_source_index_native_bincode_fixture_migrates_to_postcard() {
        let temp = tempfile::tempdir().unwrap();
        let current_path = temp.path().join(SOURCE_INDEX_FILE_NAME);
        let legacy_path = temp.path().join(LEGACY_SOURCE_INDEX_FILE_NAME);
        let mut source_index = SourceCacheIndex::new();
        source_index.insert_blob(SourceBlobEntry::new(
            "legacy-blob".to_string(),
            SourceBlobEncoding::Zstd,
            7,
            11,
            28,
            28,
        ));
        source_index.insert_aliases(
            Some("legacy:identity".to_string()),
            "https://example.test/legacy-source.webp".to_string(),
            "legacy-blob".to_string(),
        );
        let encoded = bincode::encode_to_vec(
            LegacySerializableSourceIndex::from(SerializableSourceIndex::from(&source_index)),
            LEGACY_BINCODE_CONFIG,
        )
        .unwrap();
        std::fs::write(
            &legacy_path,
            zstd::encode_all(encoded.as_slice(), 3).unwrap(),
        )
        .unwrap();

        let loaded = load_source_index(&current_path, &legacy_path)
            .await
            .unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.blob_alias_count("legacy-blob"), 2);
        assert!(current_path.exists());
    }

    #[tokio::test]
    async fn test_disk_insert_replacement_updates_byte_stats() {
        let temp = tempfile::tempdir().unwrap();
        let stats = Arc::new(CacheStats::new());
        let layer = DiskLayer::new(
            temp.path().join("cache"),
            Arc::new(BudgetManager::new()),
            stats.clone(),
        );

        layer
            .insert(
                "https://example.test/emote.webp".to_string(),
                CacheEntry::new_static("hash-a".to_string(), 100, 28, 28),
            )
            .await;
        layer
            .insert(
                "https://example.test/emote.webp".to_string(),
                CacheEntry::new_static("hash-b".to_string(), 250, 28, 28),
            )
            .await;

        assert_eq!(layer.size(), 250);
        assert_eq!(stats.disk_entries.load(Ordering::Relaxed), 1);
        assert_eq!(stats.disk_bytes.load(Ordering::Relaxed), 250);
    }

    #[tokio::test]
    async fn a_source_blob_that_cannot_be_deleted_stays_indexed_and_counted() {
        let temp = tempfile::tempdir().unwrap();
        let stats = Arc::new(CacheStats::new());
        let layer = DiskLayer::new(
            temp.path().join("cache"),
            Arc::new(BudgetManager::new()),
            stats.clone(),
        );
        let url = "https://example.test/held-open.webp";
        let blob = SourceBlobEntry::new(
            "held-blob".into(),
            SourceBlobEncoding::Zstd,
            300,
            300,
            28,
            28,
        );
        // A directory at the blob's path makes deleting it as a file fail on
        // every platform, the way a file held open does on Windows.
        let path = layer.source_blob_path(&blob.hash, blob.encoding);
        std::fs::create_dir_all(path.join("child")).unwrap();
        layer.insert_source_blob(None, url.to_string(), blob).await;
        assert_eq!(layer.size(), 300);

        layer.remove_source_aliases(None, Some(url)).await;

        assert_eq!(layer.size(), 300);
        assert_eq!(stats.disk_entries.load(Ordering::Relaxed), 1);
        assert_eq!(stats.disk_bytes.load(Ordering::Relaxed), 300);
        let source_index = layer.source_index.read().await;
        assert!(source_index.blobs.contains_key("held-blob"));
        assert_eq!(source_index.blob_alias_count("held-blob"), 0);
    }

    fn test_blob(hash: &str, size: u64) -> SourceBlobEntry {
        SourceBlobEntry::new(
            hash.to_string(),
            SourceBlobEncoding::Raw,
            size,
            size,
            28,
            28,
        )
    }

    #[tokio::test]
    async fn index_writes_and_lookups_during_the_load_see_the_installed_index() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        let persisted_url = "https://example.test/persisted.webp";
        let early_url = "https://example.test/early.webp";
        {
            let previous_run = DiskLayer::new(
                base.clone(),
                Arc::new(BudgetManager::new()),
                Arc::new(CacheStats::new()),
            );
            previous_run
                .insert_source_blob(None, persisted_url.to_string(), test_blob("persisted", 40))
                .await;
            previous_run.save_source_index_result().await.unwrap();
        }

        let stats = Arc::new(CacheStats::new());
        let layer = Arc::new(DiskLayer::new(
            base,
            Arc::new(BudgetManager::new()),
            stats.clone(),
        ));
        // Open the load window before initialize runs, as a download that
        // finishes while the index is still being read would see it.
        layer
            .index_loading
            .send_replace(Some(tokio::time::Instant::now() + INDEX_LOAD_LOOKUP_WAIT));
        let early_insert = tokio::spawn({
            let layer = Arc::clone(&layer);
            async move {
                layer
                    .insert_source_blob(None, early_url.to_string(), test_blob("early", 60))
                    .await;
            }
        });
        let early_lookup = tokio::spawn({
            let layer = Arc::clone(&layer);
            async move { layer.get_source_entry(None, persisted_url).await }
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!early_insert.is_finished(), "the insert waits for the load");
        assert!(!early_lookup.is_finished(), "the lookup waits for the load");

        layer.initialize().await.unwrap();
        early_insert.await.unwrap();
        let looked_up = early_lookup.await.unwrap();

        assert_eq!(
            looked_up.map(|entry| entry.hash),
            Some("persisted".to_string())
        );
        let source_index = layer.source_index.read().await;
        assert!(source_index.blobs.contains_key("persisted"));
        assert!(
            source_index.blobs.contains_key("early"),
            "installing the loaded index must not drop an insert made during the load"
        );
        drop(source_index);
        assert_eq!(layer.size(), 100);
        assert_eq!(stats.disk_entries.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn lookups_give_up_on_a_stalled_index_load() {
        let temp = tempfile::tempdir().unwrap();
        let layer = DiskLayer::new(
            temp.path().join("cache"),
            Arc::new(BudgetManager::new()),
            Arc::new(CacheStats::new()),
        );
        let started = std::time::Instant::now();
        layer
            .index_loading
            .send_replace(Some(tokio::time::Instant::now() + INDEX_LOAD_LOOKUP_WAIT));

        let missing = "https://example.test/missing.webp";
        assert!(layer.get_source_entry(None, missing).await.is_none());
        let waited = started.elapsed();
        assert!(waited >= INDEX_LOAD_LOOKUP_WAIT);
        assert!(waited < INDEX_LOAD_LOOKUP_WAIT * 5);

        // The window is shared: a second lookup does not wait it out again.
        let second = std::time::Instant::now();
        assert!(layer.get_entry(missing).await.is_none());
        assert!(second.elapsed() < INDEX_LOAD_LOOKUP_WAIT / 2);
    }

    async fn saved_source_blobs(layer: &DiskLayer) -> Vec<String> {
        let saved = load_source_index(
            &layer.source_index_path(),
            &layer.legacy_source_index_path(),
        )
        .await
        .unwrap();
        let mut hashes: Vec<String> = saved.blobs.keys().cloned().collect();
        hashes.sort();
        hashes
    }

    /// Lets spawned autosaves run until `done` holds, or fails after 5 s.
    async fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn an_autosave_neither_blocks_the_insert_nor_repeats_a_running_save() {
        let temp = tempfile::tempdir().unwrap();
        let layer = DiskLayer::new(
            temp.path().join("cache"),
            Arc::new(BudgetManager::new()),
            Arc::new(CacheStats::new()),
        );
        // Every write is due by time, as in a burst long after the last save.
        layer
            .source_index_save
            .last_save_ts
            .store(0, Ordering::Relaxed);

        // Another save holds the lock for the whole burst.
        let running_save = layer.source_index_save.lock.lock().await;
        for hash in ["a", "b", "c"] {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                layer.insert_source_blob(
                    None,
                    format!("https://example.test/{hash}.webp"),
                    test_blob(hash, 10),
                ),
            )
            .await
            .expect("the insert does not wait for the save");
        }
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(
            !layer.source_index_path().exists(),
            "autosaves skip while a save is running"
        );
        assert_eq!(
            layer.source_index_save.dirty_writes.load(Ordering::Relaxed),
            3,
            "the skipped writes stay counted"
        );
        drop(running_save);

        layer
            .insert_source_blob(
                None,
                "https://example.test/d.webp".to_string(),
                test_blob("d", 10),
            )
            .await;
        wait_until(|| layer.source_index_save.dirty_writes.load(Ordering::Relaxed) == 0).await;
        assert_ne!(
            layer.source_index_save.last_save_ts.load(Ordering::Relaxed),
            0
        );
        assert_eq!(saved_source_blobs(&layer).await, ["a", "b", "c", "d"]);
    }

    #[tokio::test]
    async fn an_autosave_during_the_index_load_saves_the_installed_index() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        {
            let previous_run = DiskLayer::new(
                base.clone(),
                Arc::new(BudgetManager::new()),
                Arc::new(CacheStats::new()),
            );
            previous_run
                .insert_source_blob(
                    None,
                    "https://example.test/persisted.webp".to_string(),
                    test_blob("persisted", 40),
                )
                .await;
            previous_run.save_source_index_result().await.unwrap();
        }

        let layer = DiskLayer::new(
            base,
            Arc::new(BudgetManager::new()),
            Arc::new(CacheStats::new()),
        );
        layer
            .source_index_save
            .last_save_ts
            .store(0, Ordering::Relaxed);
        layer
            .index_loading
            .send_replace(Some(tokio::time::Instant::now() + INDEX_LOAD_LOOKUP_WAIT));
        // A touch before the load installs the index triggers an autosave of
        // the still-empty index.
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            layer.maybe_autosave_source_index(),
        )
        .await
        .expect("the autosave does not wait for the load");
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            saved_source_blobs(&layer).await,
            ["persisted"],
            "nothing is saved while the load runs"
        );

        layer.initialize().await.unwrap();
        wait_until(|| layer.source_index_save.last_save_ts.load(Ordering::Relaxed) != 0).await;
        assert_eq!(saved_source_blobs(&layer).await, ["persisted"]);
    }
}
