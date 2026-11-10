//! Remembered native pixel sizes of decoded chat media.
//!
//! Chat layout sizes Twitch/BTTV emotes, Twitch GIFs and embed thumbnails from
//! the decoded image. Idle decoded media is swept after a few minutes, so a
//! later rebuild of the same row would otherwise fall back to provider
//! metadata (BTTV defaults to 28x28) and re-wrap twice. Sizes observed once are
//! kept here, bounded, and persisted so first layouts after a restart match.

use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_ENTRIES: usize = 16_384;
const FILE_NAME: &str = "media_native_sizes.bin";
const MAGIC: &[u8; 8] = b"ECHONSZ1";
const ENTRY_BYTES: usize = 12;

#[derive(Default)]
struct Sizes {
    by_url: FxHashMap<u64, (u16, u16)>,
    insertion_order: VecDeque<u64>,
}

impl Sizes {
    fn insert(&mut self, key: u64, size: (u16, u16)) -> bool {
        match self.by_url.insert(key, size) {
            Some(previous) => previous != size,
            None => {
                self.insertion_order.push_back(key);
                while self.insertion_order.len() > MAX_ENTRIES {
                    if let Some(oldest) = self.insertion_order.pop_front() {
                        self.by_url.remove(&oldest);
                    }
                }
                true
            }
        }
    }
}

static SIZES: LazyLock<RwLock<Sizes>> = LazyLock::new(|| RwLock::new(Sizes::default()));
static DIRTY: AtomicBool = AtomicBool::new(false);
static SAVE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn stable_url_key(url: &str) -> u64 {
    url.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn path() -> PathBuf {
    crate::benchmark_profile::config_root_or_current_dir()
        .join("cache")
        .join(FILE_NAME)
}

/// Record the decoded pixel size of `url`.
pub fn remember(url: &str, width: u32, height: u32) {
    let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
        return;
    };
    if width == 0 || height == 0 {
        return;
    }
    let key = stable_url_key(url);
    if SIZES.read().by_url.get(&key) == Some(&(width, height)) {
        return;
    }
    if SIZES.write().insert(key, (width, height)) {
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// The decoded pixel size last observed for `url`, if any.
pub fn remembered(url: &str) -> Option<(u32, u32)> {
    SIZES
        .read()
        .by_url
        .get(&stable_url_key(url))
        .map(|&(width, height)| (u32::from(width), u32::from(height)))
}

fn decode(bytes: &[u8]) -> Option<Vec<(u64, (u16, u16))>> {
    let body = bytes.strip_prefix(MAGIC.as_slice())?;
    let (count, entries) = body.split_first_chunk::<4>()?;
    let count = u32::from_le_bytes(*count) as usize;
    if count > MAX_ENTRIES || entries.len() != count * ENTRY_BYTES {
        return None;
    }
    Some(
        entries
            .as_chunks::<ENTRY_BYTES>()
            .0
            .iter()
            .map(|entry| {
                let key = u64::from_le_bytes(entry[0..8].try_into().unwrap_or_default());
                let width = u16::from_le_bytes([entry[8], entry[9]]);
                let height = u16::from_le_bytes([entry[10], entry[11]]);
                (key, (width, height))
            })
            .filter(|(_, (width, height))| *width > 0 && *height > 0)
            .collect(),
    )
}

fn encode(entries: &[(u64, (u16, u16))]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + entries.len() * ENTRY_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, (width, height)) in entries {
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
    }
    bytes
}

/// Load persisted sizes. Sizes observed this session before the load win.
pub async fn load() {
    let path = path();
    let loaded = tokio::task::spawn_blocking(move || std::fs::read(&path).ok())
        .await
        .ok()
        .flatten()
        .and_then(|bytes| decode(&bytes));
    let Some(entries) = loaded else {
        return;
    };
    let mut sizes = SIZES.write();
    let observed = std::mem::take(&mut *sizes);
    for (key, size) in entries {
        sizes.insert(key, size);
    }
    for key in observed.insertion_order {
        if let Some(&size) = observed.by_url.get(&key) {
            sizes.insert(key, size);
        }
    }
}

/// Persist the sizes when they changed since the last save.
pub async fn save() {
    if !DIRTY.swap(false, Ordering::Relaxed) {
        return;
    }
    let result = tokio::task::spawn_blocking(|| {
        let _save = SAVE_LOCK.lock();
        let entries: Vec<(u64, (u16, u16))> = {
            let sizes = SIZES.read();
            sizes
                .insertion_order
                .iter()
                .filter_map(|key| sizes.by_url.get(key).map(|&size| (*key, size)))
                .collect()
        };
        write_atomically(&path(), &encode(&entries))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result.map_err(|error| error.to_string()));
    if let Err(error) = result {
        DIRTY.store(true, Ordering::Relaxed);
        crate::log_cache!(debug, "Failed to save media native sizes: {}", error);
    }
}

fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("bin.tmp");
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| crate::utils::replace_file(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn encoding_round_trips_and_rejects_truncation() {
        let entries = vec![(1, (112, 112)), (u64::MAX, (84, 28))];
        let bytes = encode(&entries);
        assert_eq!(decode(&bytes), Some(entries));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(b"NOTMAGIC\0\0\0\0"), None);
    }

    #[test]
    fn bounded_insertion_evicts_oldest() {
        let mut sizes = Sizes::default();
        for key in 0..(MAX_ENTRIES as u64 + 2) {
            sizes.insert(key, (28, 28));
        }
        assert_eq!(sizes.by_url.len(), MAX_ENTRIES);
        assert!(!sizes.by_url.contains_key(&0));
        assert!(!sizes.by_url.contains_key(&1));
        assert!(!sizes.insert(5, (28, 28)));
        assert!(sizes.insert(5, (56, 28)));
    }
}
