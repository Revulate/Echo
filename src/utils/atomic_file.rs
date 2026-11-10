//! Platform-correct replacement of an existing file by a sibling temporary file.

use std::io;
use std::path::{Path, PathBuf};

/// Synchronize `replacement`, then move it over `destination` without deleting the last
/// good destination first; see [`echo_core::file_replace::replace_file`].
pub(crate) fn replace_file(replacement: &Path, destination: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(replacement)?
        .sync_all()?;
    echo_core::file_replace::replace_file(replacement, destination)
}

pub(crate) fn backup_generation_paths(path: &Path) -> [PathBuf; 3] {
    std::array::from_fn(|generation| {
        let mut name = path.as_os_str().to_os_string();
        name.push(if generation == 0 {
            ".bak".to_string()
        } else {
            format!(".bak.{generation}")
        });
        PathBuf::from(name)
    })
}

pub(crate) fn rotate_known_good_backups(
    path: &Path,
    validate: impl Fn(&[u8]) -> bool,
) -> io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let primary = std::fs::read(path)?;
    if !validate(&primary) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "primary file is not valid",
        ));
    }
    let paths = backup_generation_paths(path);
    let mut snapshots = vec![primary];
    for backup in &paths {
        match std::fs::read(backup) {
            Ok(bytes) if validate(&bytes) => {
                if snapshots.len() < paths.len() && !snapshots.contains(&bytes) {
                    snapshots.push(bytes);
                }
            }
            Ok(_) => {
                quarantine_corrupt_file(backup)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for (backup, bytes) in paths.iter().zip(snapshots.iter()).rev() {
        if std::fs::read(backup).is_ok_and(|existing| existing == *bytes) {
            continue;
        }
        let mut temporary = backup.as_os_str().to_os_string();
        temporary.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = PathBuf::from(temporary);
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            drop(file);
            replace_file(&temporary, backup)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
    }
    Ok(())
}

/// Quarantine copies kept per file; older ones are pruned.
const MAX_QUARANTINE_COPIES: usize = 3;

/// Copy a file that failed to load to `<name>.corrupt-<unix seconds>` beside it,
/// so a later save cannot destroy the only copy of the user's data. Keeps the
/// newest few quarantine copies for that file name. When the newest existing
/// copy already holds identical bytes, that copy is returned instead.
pub(crate) fn quarantine_corrupt_file(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("file has no UTF-8 name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;

    if let Some((_, newest)) = quarantine_copies(parent, file_name).into_iter().next()
        && files_have_same_contents(path, &newest)
    {
        return Ok(newest);
    }

    let mut timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let quarantine_path = loop {
        let candidate = parent.join(format!("{file_name}.corrupt-{timestamp}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut copy) => {
                let result = (|| {
                    let mut source = std::fs::File::open(path)?;
                    std::io::copy(&mut source, &mut copy)?;
                    copy.sync_all()
                })();
                if let Err(error) = result {
                    drop(copy);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                break candidate;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                timestamp = timestamp
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("quarantine timestamp exhausted"))?;
            }
            Err(error) => return Err(error),
        }
    };

    for stale in stale_quarantine_copies(quarantine_copies(parent, file_name), &quarantine_path) {
        let _ = std::fs::remove_file(stale);
    }

    Ok(quarantine_path)
}

/// Quarantine copies of `file_name` in `parent`, newest first.
fn quarantine_copies(parent: &Path, file_name: &str) -> Vec<(u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut copies: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let timestamp = quarantine_timestamp(file_name, name.to_str()?)?;
            Some((timestamp, entry.path()))
        })
        .collect();
    copies.sort_unstable_by_key(|(timestamp, _)| std::cmp::Reverse(*timestamp));
    copies
}

/// Copies past the newest `MAX_QUARANTINE_COPIES` (newest-first input), never
/// including `keep`, the copy just written.
fn stale_quarantine_copies(copies: Vec<(u64, PathBuf)>, keep: &Path) -> Vec<PathBuf> {
    copies
        .into_iter()
        .skip(MAX_QUARANTINE_COPIES)
        .map(|(_, path)| path)
        .filter(|path| path != keep)
        .collect()
}

fn files_have_same_contents(a: &Path, b: &Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if a_meta.len() != b_meta.len() {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a_bytes), Ok(b_bytes)) => a_bytes == b_bytes,
        _ => false,
    }
}

/// Preserve a file that failed to load before anything overwrites it. Returns
/// whether `path` may now be overwritten: it was copied aside, or it is gone.
pub(crate) fn preserve_unloadable_file(path: &Path, what: &str) -> bool {
    if !path.exists() {
        return true;
    }
    match quarantine_corrupt_file(path) {
        Ok(quarantine_path) => {
            crate::log_persist!(
                warn,
                "Preserved the unloadable {} as '{}'",
                what,
                quarantine_path.display()
            );
            true
        }
        Err(error) => {
            crate::log_persist!(
                error,
                "Could not preserve the unloadable {} '{}': {}; it will not be overwritten",
                what,
                path.display(),
                error
            );
            false
        }
    }
}

/// Where and how a JSON document failed to parse, without echoing any of its
/// content (settings and token files can hold secrets).
pub(crate) fn json_error_summary(error: &serde_json::Error) -> String {
    format!(
        "{:?} error at line {} column {}",
        error.classify(),
        error.line(),
        error.column()
    )
}

/// The timestamp of `<file_name>.corrupt-<unix seconds>`, if `candidate` is one.
fn quarantine_timestamp(file_name: &str, candidate: &str) -> Option<u64> {
    let digits = candidate
        .strip_prefix(file_name)?
        .strip_prefix(".corrupt-")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn quarantine_timestamp_matches_only_copies_of_the_same_file() {
        assert_eq!(
            quarantine_timestamp("settings.json", "settings.json.corrupt-1700000000"),
            Some(1_700_000_000)
        );
        assert_eq!(
            quarantine_timestamp("settings.json", "settings.json.bak.corrupt-1700000000"),
            None
        );
        assert_eq!(
            quarantine_timestamp("settings.json.bak", "settings.json.bak.corrupt-5"),
            Some(5)
        );
        assert_eq!(
            quarantine_timestamp("settings.json", "settings.json.corrupt-"),
            None
        );
        assert_eq!(
            quarantine_timestamp("settings.json", "settings.json.corrupt-12a"),
            None
        );
        assert_eq!(quarantine_timestamp("settings.json", "settings.json"), None);
    }

    #[test]
    fn stale_quarantine_copies_never_include_the_kept_copy() {
        let copies: Vec<(u64, PathBuf)> = (0..5u64)
            .rev()
            .map(|n| (n, PathBuf::from(format!("s.json.corrupt-{n}"))))
            .collect();
        assert_eq!(
            stale_quarantine_copies(copies.clone(), Path::new("s.json.corrupt-9")),
            vec![
                PathBuf::from("s.json.corrupt-1"),
                PathBuf::from("s.json.corrupt-0")
            ]
        );
        // A just-written copy with an older-looking timestamp (clock moved
        // backwards) must survive pruning.
        assert_eq!(
            stale_quarantine_copies(copies, Path::new("s.json.corrupt-0")),
            vec![PathBuf::from("s.json.corrupt-1")]
        );
    }

    #[test]
    fn quarantine_reuses_identical_newest_copy() {
        // No tempfile: tools/layout-persistence-check compiles this file too.
        let dir = std::env::temp_dir().join(format!(
            "echo-quarantine-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tabs.json");
        std::fs::write(&path, b"{broken").unwrap();
        let first = quarantine_corrupt_file(&path).unwrap();
        let second = quarantine_corrupt_file(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(quarantine_copies(&dir, "tabs.json").len(), 1);

        std::fs::write(&path, b"{also broken").unwrap();
        let third = quarantine_corrupt_file(&path).unwrap();
        assert_eq!(std::fs::read(&third).unwrap(), b"{also broken");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
