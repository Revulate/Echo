//! Bounded reads and atomic writes for the cache's persisted files.
//!
//! Every file here is read back at startup from a directory any process can
//! write to, so reads are capped before and after decompression, and writes
//! go through a synchronized sibling temporary so a crash never leaves a
//! truncated file in place of the last good one.

use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest zstd window accepted when decoding (32 MiB). It bounds the
/// decoder's own allocation independently of the output cap.
pub const ZSTD_WINDOW_LOG_MAX: u32 = 25;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read at most `limit` bytes from `reader`, failing when there are more.
///
/// File metadata can change between inspection and read, so the cap is
/// applied to what is actually read: at most `limit + 1` bytes are captured.
pub fn read_capped(mut reader: impl Read, limit: usize, label: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid_data(format!(
            "compressed {label} exceeds {limit} byte limit"
        )));
    }
    Ok(bytes)
}

/// Read and decompress a zstd file, capping both the compressed input and
/// the decompressed output.
pub fn read_zstd_bounded(
    path: &Path,
    max_compressed: usize,
    max_decompressed: usize,
    label: &str,
) -> io::Result<Vec<u8>> {
    let compressed = read_capped(std::fs::File::open(path)?, max_compressed, label)?;
    let mut decoder = zstd::Decoder::new(Cursor::new(compressed))?;
    decoder.window_log_max(ZSTD_WINDOW_LOG_MAX)?;
    let mut data = Vec::new();
    decoder
        .by_ref()
        .take(max_decompressed as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() > max_decompressed {
        return Err(invalid_data(format!(
            "decompressed {label} exceeds {max_decompressed} byte limit"
        )));
    }
    Ok(data)
}

/// A unique temporary path next to `path`, so the final rename stays on one
/// filesystem and concurrent writers never share a temporary file.
pub fn temp_path_for(path: &Path) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let process_id = std::process::id();
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!("{ext}.tmp-{process_id}-{counter}"))
        .unwrap_or_else(|| format!("tmp-{process_id}-{counter}"));
    path.with_extension(extension)
}

/// Write `path` atomically: `write` fills a fresh temporary file, which is
/// synchronized and then moved over `path`. The temporary is removed when any
/// step fails.
pub fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let temporary = temp_path_for(path);
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        echo_core::file_replace::replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_reads_reject_over_limit_input() {
        assert!(read_capped(Cursor::new([1, 2, 3]), 2, "test").is_err());
        assert_eq!(read_capped(Cursor::new([1, 2]), 2, "test").unwrap(), [1, 2]);
    }

    #[test]
    fn temp_paths_are_unique_and_adjacent() {
        let path = Path::new("cache").join("index.postcard.zst");
        let a = temp_path_for(&path);
        let b = temp_path_for(&path);
        assert_ne!(a, b);
        assert_eq!(a.parent(), path.parent());
        assert!(
            a.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".zst.tmp-"))
        );
    }

    #[test]
    fn atomic_writes_replace_and_bounded_reads_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.zst");
        let payload = vec![7u8; 4096];
        for _ in 0..2 {
            write_atomically(&path, |file| {
                zstd::stream::copy_encode(payload.as_slice(), file, 3)
            })
            .unwrap();
        }
        assert_eq!(
            read_zstd_bounded(&path, 1 << 20, 4096, "test").unwrap(),
            payload
        );
        assert!(read_zstd_bounded(&path, 1 << 20, 4095, "test").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
