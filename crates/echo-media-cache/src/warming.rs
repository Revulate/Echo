use bincode::Decode;
#[cfg(test)]
use bincode::Encode;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PRIORITY_ENTRIES: usize = 500;
const MIN_ACCESS_THRESHOLD: u64 = 3;
pub const WARMING_CACHE_FILE: &str = "cache_warming.postcard.zst";
const LEGACY_WARMING_CACHE_FILE: &str = "cache_warming.bincode.zst";
const CACHE_VERSION: u32 = 1;
const MAX_WARMING_COMPRESSED_BYTES: u64 = 1024 * 1024;
const MAX_WARMING_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessStats {
    pub access_count: u64,
    pub last_access: u64,
    pub size_bytes: u32,
    pub is_animated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WarmingPriorities {
    pub version: u32,
    pub entries: HashMap<String, AccessStats>,
    pub last_saved: u64,
}

// Frozen Bincode-v1 mirrors keep compatibility bytes isolated from the
// current Serde/Postcard warming schema.
#[derive(Debug, Clone, Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacyAccessStats {
    access_count: u64,
    last_access: u64,
    size_bytes: u32,
    is_animated: bool,
}

#[derive(Debug, Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacyWarmingPriorities {
    version: u32,
    entries: HashMap<String, LegacyAccessStats>,
    last_saved: u64,
}

impl From<LegacyAccessStats> for AccessStats {
    fn from(value: LegacyAccessStats) -> Self {
        Self {
            access_count: value.access_count,
            last_access: value.last_access,
            size_bytes: value.size_bytes,
            is_animated: value.is_animated,
        }
    }
}

#[cfg(test)]
impl From<AccessStats> for LegacyAccessStats {
    fn from(value: AccessStats) -> Self {
        Self {
            access_count: value.access_count,
            last_access: value.last_access,
            size_bytes: value.size_bytes,
            is_animated: value.is_animated,
        }
    }
}

impl From<LegacyWarmingPriorities> for WarmingPriorities {
    fn from(value: LegacyWarmingPriorities) -> Self {
        Self {
            version: value.version,
            entries: value
                .entries
                .into_iter()
                .map(|(url, stats)| (url, stats.into()))
                .collect(),
            last_saved: value.last_saved,
        }
    }
}

#[cfg(test)]
impl From<WarmingPriorities> for LegacyWarmingPriorities {
    fn from(value: WarmingPriorities) -> Self {
        Self {
            version: value.version,
            entries: value
                .entries
                .into_iter()
                .map(|(url, stats)| (url, stats.into()))
                .collect(),
            last_saved: value.last_saved,
        }
    }
}

impl Default for WarmingPriorities {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            entries: HashMap::new(),
            last_saved: 0,
        }
    }
}

pub struct WarmingTracker {
    path: PathBuf,
    access_stats: DashMap<String, AccessStats>,
    loaded: AtomicBool,
    dirty: AtomicBool,
}

impl WarmingTracker {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            access_stats: DashMap::with_capacity(1000),
            loaded: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
        }
    }

    pub async fn load_priorities(&self) {
        if self.loaded.swap(true, Ordering::SeqCst) {
            return;
        }
        let path = self.path.clone();
        let result = tokio::task::spawn_blocking(move || load_from_current_or_legacy(&path)).await;
        match result {
            Ok(Ok(WarmingLoad::Current(priorities))) => {
                for (url, stats) in priorities.entries {
                    self.access_stats.insert(url, stats);
                }
                crate::log_cache!(
                    info,
                    count = self.access_stats.len(),
                    "Loaded warming priorities"
                );
            }
            Ok(Ok(WarmingLoad::Legacy(priorities))) => {
                for (url, stats) in priorities.entries {
                    self.access_stats.insert(url, stats);
                }
                crate::log_cache!(
                    info,
                    count = self.access_stats.len(),
                    "Recovered warming priorities from retired Bincode format"
                );
            }
            Ok(Ok(WarmingLoad::Missing)) => {
                crate::log_cache!(debug, "No warming cache found, starting fresh")
            }
            Ok(Err(error)) => {
                crate::log_cache!(warn, error = %error, "Failed to load warming cache")
            }
            Err(error) => crate::log_cache!(warn, error = %error, "Cache warming load task failed"),
        }
    }

    pub async fn save_priorities(&self) {
        self.save_priorities_at(now_secs()).await;
    }

    pub async fn save_priorities_at(&self, now: u64) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let mut entries: Vec<_> = self
            .access_stats
            .iter()
            .filter(|entry| entry.value().access_count >= MIN_ACCESS_THRESHOLD)
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        entries.sort_by(|a, b| warming_score(&b.1, now).total_cmp(&warming_score(&a.1, now)));
        entries.truncate(MAX_PRIORITY_ENTRIES);
        let priorities = WarmingPriorities {
            version: CACHE_VERSION,
            entries: entries.into_iter().collect(),
            last_saved: now,
        };
        let path = self.path.clone();
        let count = priorities.entries.len();
        match tokio::task::spawn_blocking(move || save_to_file(&path, &priorities)).await {
            Ok(Ok(())) => crate::log_cache!(debug, count, "Saved warming priorities"),
            Ok(Err(error)) => {
                crate::log_cache!(warn, error = %error, "Failed to save warming cache")
            }
            Err(error) => crate::log_cache!(warn, error = %error, "Cache warming save task failed"),
        }
    }

    pub fn top_urls(&self, limit: usize) -> Vec<String> {
        self.top_urls_at(limit, now_secs())
    }

    pub fn top_urls_at(&self, limit: usize, now: u64) -> Vec<String> {
        if limit == 0 || !self.loaded.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut rows: Vec<_> = self
            .access_stats
            .iter()
            .filter(|entry| entry.value().access_count >= MIN_ACCESS_THRESHOLD)
            .map(|entry| (entry.key().clone(), warming_score(entry.value(), now)))
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        rows.truncate(limit);
        rows.into_iter().map(|(url, _)| url).collect()
    }

    pub fn record_access(&self, url: &str, size_bytes: u32, is_animated: bool) {
        self.record_access_at(url, size_bytes, is_animated, now_secs());
    }

    pub fn record_access_at(&self, url: &str, size_bytes: u32, is_animated: bool, now: u64) {
        if url.is_empty() {
            return;
        }
        self.access_stats
            .entry(url.to_string())
            .and_modify(|stats| {
                stats.access_count = stats.access_count.saturating_add(1);
                stats.last_access = now;
                if size_bytes > 0 {
                    stats.size_bytes = size_bytes;
                }
                stats.is_animated |= is_animated;
            })
            .or_insert(AccessStats {
                access_count: 1,
                last_access: now,
                size_bytes,
                is_animated,
            });
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn get_access_count(&self, url: &str) -> u64 {
        self.access_stats
            .get(url)
            .map_or(0, |entry| entry.access_count)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn warming_score(stats: &AccessStats, now: u64) -> f64 {
    let age_days = now.saturating_sub(stats.last_access) as f64 / 86_400.0;
    let recency = 0.5_f64.powf(age_days / 7.0);
    let size_kb = stats.size_bytes as f64 / 1024.0;
    let size_factor = if size_kb < 4.0 {
        1.0
    } else if size_kb < 16.0 {
        0.9
    } else if size_kb < 64.0 {
        0.7
    } else {
        0.5
    };
    let animated_boost = if stats.is_animated { 1.2 } else { 1.0 };
    (stats.access_count as f64).ln_1p() * recency * size_factor * animated_boost
}

enum WarmingLoad {
    Current(WarmingPriorities),
    Legacy(WarmingPriorities),
    Missing,
}

fn load_from_current_or_legacy(path: &Path) -> Result<WarmingLoad, String> {
    match fs::metadata(path) {
        Ok(_) => load_postcard_file(path).map(WarmingLoad::Current),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let legacy_path = legacy_path_for(path);
            match fs::metadata(&legacy_path) {
                Ok(_) => {
                    let priorities = load_legacy_bincode_file(&legacy_path)?;
                    if let Err(error) = save_to_file(path, &priorities) {
                        crate::log_cache!(
                            warn,
                            path = %path.display(),
                            error = %error,
                            "Loaded legacy warming cache but could not publish Postcard migration"
                        );
                    }
                    Ok(WarmingLoad::Legacy(priorities))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WarmingLoad::Missing),
                Err(error) => Err(format!("Legacy cache metadata error: {error}")),
            }
        }
        Err(error) => Err(format!("Cache metadata error: {error}")),
    }
}

fn load_postcard_file(path: &Path) -> Result<WarmingPriorities, String> {
    let decompressed = read_zstd_limited(path)?;
    let (priorities, remaining): (WarmingPriorities, _) = postcard::take_from_bytes(&decompressed)
        .map_err(|error| format!("Postcard decode error: {error}"))?;
    if !remaining.is_empty() {
        return Err("Postcard payload has trailing bytes".to_string());
    }
    validate_warming_version(&priorities)?;
    Ok(priorities)
}

fn load_legacy_bincode_file(path: &Path) -> Result<WarmingPriorities, String> {
    let decompressed = read_zstd_limited(path)?;
    let (priorities, consumed): (LegacyWarmingPriorities, _) = bincode::decode_from_slice(
        &decompressed,
        bincode::config::standard().with_limit::<{ MAX_WARMING_DECOMPRESSED_BYTES }>(),
    )
    .map_err(|error| format!("Legacy Bincode decode error: {error}"))?;
    if consumed != decompressed.len() {
        return Err("Legacy Bincode payload has trailing bytes".to_string());
    }
    let priorities = WarmingPriorities::from(priorities);
    validate_warming_version(&priorities)?;
    Ok(priorities)
}

fn validate_warming_version(priorities: &WarmingPriorities) -> Result<(), String> {
    if priorities.version != CACHE_VERSION {
        return Err(format!(
            "Version mismatch: expected {CACHE_VERSION}, got {}",
            priorities.version
        ));
    }
    Ok(())
}

fn save_to_file(path: &Path, priorities: &WarmingPriorities) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("Mkdir error: {error}"))?;
    }
    let encoded = postcard::to_stdvec(priorities)
        .map_err(|error| format!("Postcard encode error: {error}"))?;
    let compressed = zstd::encode_all(encoded.as_slice(), 3)
        .map_err(|error| format!("Compress error: {error}"))?;
    write_atomically(path, &compressed).map_err(|error| format!("Write error: {error}"))
}

fn read_zstd_limited(path: &Path) -> Result<Vec<u8>, String> {
    // Capture a bounded immutable compressed payload before decoding; file
    // metadata cannot safely constrain a file that grows while being read.
    let compressed = read_compressed_bytes_limited(
        fs::File::open(path).map_err(|error| format!("Read error: {error}"))?,
        MAX_WARMING_COMPRESSED_BYTES as usize,
    )?;
    let mut decoder = zstd::Decoder::new(Cursor::new(compressed))
        .map_err(|error| format!("Decompress error: {error}"))?;
    decoder
        .window_log_max(25)
        .map_err(|error| format!("Decompress window error: {error}"))?;
    let mut decompressed = Vec::new();
    decoder
        .by_ref()
        .take(MAX_WARMING_DECOMPRESSED_BYTES as u64 + 1)
        .read_to_end(&mut decompressed)
        .map_err(|error| format!("Decompress error: {error}"))?;
    if decompressed.len() > MAX_WARMING_DECOMPRESSED_BYTES {
        return Err(format!(
            "Decompressed warming cache exceeds {MAX_WARMING_DECOMPRESSED_BYTES} byte limit"
        ));
    }
    Ok(decompressed)
}

fn read_compressed_bytes_limited(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut compressed = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut compressed)
        .map_err(|error| format!("Read error: {error}"))?;
    if compressed.len() > limit {
        return Err(format!(
            "Compressed warming cache exceeds {limit} byte limit"
        ));
    }
    Ok(compressed)
}

fn legacy_path_for(current_path: &Path) -> PathBuf {
    current_path.with_file_name(LEGACY_WARMING_CACHE_FILE)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path_for(path);
    let write_result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        echo_core::file_replace::replace_file(&temporary, path)
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn temporary_path_for(path: &Path) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!("{extension}.tmp-{}-{counter}", std::process::id()))
        .unwrap_or_else(|| format!("tmp-{}-{counter}", std::process::id()));
    path.with_extension(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warming_score() {
        let now = 2_000_000_000;
        let stats = AccessStats {
            access_count: 10,
            last_access: now,
            size_bytes: 4096,
            is_animated: false,
        };
        assert!(warming_score(&stats, now) > 0.0);
    }

    #[test]
    fn test_access_recording() {
        let temp = tempfile::tempdir().unwrap();
        let tracker = WarmingTracker::new(temp.path().join(WARMING_CACHE_FILE));
        tracker.record_access_at("https://test.com/emote.png", 1024, false, 2_000_000_000);
        assert_eq!(tracker.get_access_count("https://test.com/emote.png"), 1);
    }

    #[tokio::test]
    async fn record_before_load_does_not_skip_persisted_priorities() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(WARMING_CACHE_FILE);
        let persisted_url = "https://test.com/persisted.png";
        let priorities = WarmingPriorities {
            version: CACHE_VERSION,
            entries: HashMap::from([(
                persisted_url.to_string(),
                AccessStats {
                    access_count: 4,
                    last_access: 1_999_999_000,
                    size_bytes: 2048,
                    is_animated: true,
                },
            )]),
            last_saved: 1_999_999_000,
        };
        save_to_file(&path, &priorities).unwrap();

        let tracker = WarmingTracker::new(path);
        let recorded_url = "https://test.com/recorded-before-load.png";
        tracker.record_access_at(recorded_url, 1024, false, 2_000_000_000);
        tracker.load_priorities().await;

        assert_eq!(tracker.get_access_count(recorded_url), 1);
        assert_eq!(tracker.get_access_count(persisted_url), 4);
    }

    #[test]
    fn postcard_warming_payload_rejects_trailing_bytes() {
        let priorities = WarmingPriorities::default();
        let mut encoded = postcard::to_stdvec(&priorities).unwrap();
        encoded.push(0);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(WARMING_CACHE_FILE);
        fs::write(&path, zstd::encode_all(encoded.as_slice(), 3).unwrap()).unwrap();

        assert!(load_postcard_file(&path).is_err());
    }

    #[test]
    fn compressed_warming_capture_rejects_over_limit_input() {
        assert!(read_compressed_bytes_limited(Cursor::new([1, 2, 3]), 2).is_err());
    }

    #[test]
    fn legacy_warming_file_is_used_only_when_current_file_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        let current_path = temp.path().join(WARMING_CACHE_FILE);
        let legacy_path = legacy_path_for(&current_path);
        let priorities = WarmingPriorities {
            version: CACHE_VERSION,
            entries: HashMap::from([(
                "https://test.example/legacy.png".to_string(),
                AccessStats {
                    access_count: 4,
                    last_access: 1_999_999_000,
                    size_bytes: 1024,
                    is_animated: false,
                },
            )]),
            last_saved: 1_999_999_000,
        };
        let legacy = bincode::encode_to_vec(
            LegacyWarmingPriorities::from(priorities),
            bincode::config::standard(),
        )
        .unwrap();
        fs::write(
            &legacy_path,
            zstd::encode_all(legacy.as_slice(), 3).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            load_from_current_or_legacy(&current_path).unwrap(),
            WarmingLoad::Legacy(loaded) if loaded.entries.len() == 1
        ));
        assert!(current_path.exists());

        fs::write(
            &current_path,
            zstd::encode_all(&b"not a postcard"[..], 3).unwrap(),
        )
        .unwrap();
        assert!(load_from_current_or_legacy(&current_path).is_err());
    }
}
