//! Refill sources for animated emotes.
//!
//! An animation's source is every frame as zstd-compressed BGRA. It feeds the
//! bounded hot-frame window and rebuilds a hot frame whose CPU pixels were
//! released after its atlas upload. Sources start in RAM straight from the
//! decoder and are then written to a session-scoped sidecar file, after which
//! only the frame table stays in memory and frames are read back with
//! positional reads.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::image_decode::CompressedDecodedFrameData;

/// Where one frame's compressed bytes live in a sidecar.
struct SidecarFrame {
    offset: u64,
    compressed_len: u32,
    raw_len: u32,
    width: u32,
    height: u32,
}

/// An animation's frames written to a sidecar file. The file is deleted when
/// the last holder (the source cache or a hot frame that may rebuild) drops it.
pub(super) struct Sidecar {
    file: File,
    /// Deleted on drop where the file is not opened delete-on-close.
    #[cfg_attr(windows, allow(dead_code))]
    path: PathBuf,
    frames: Box<[SidecarFrame]>,
    file_bytes: u64,
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        SIDECAR_DISK_BYTES.fetch_sub(self.file_bytes, Ordering::Relaxed);
        // On Windows the file is opened delete-on-close, so closing the handle
        // removes it, also when the process ends without running this.
        #[cfg(not(windows))]
        {
            let path = std::mem::take(&mut self.path);
            // Hot frames can drop on the UI thread.
            crate::backend::bridge::get_runtime().spawn_blocking(move || {
                let _ = std::fs::remove_file(path);
            });
        }
    }
}

/// Bytes in every live sidecar file, including sources the cache no longer
/// tracks that hot frames still hold to rebuild released pixels.
pub(super) fn sidecar_disk_bytes() -> u64 {
    SIDECAR_DISK_BYTES.load(Ordering::Relaxed)
}

enum Storage {
    Memory(Box<[CompressedDecodedFrameData]>),
    Disk(Sidecar),
}

/// One animation's compressed frames, in RAM or in a sidecar file.
pub(super) struct AnimationSource {
    storage: RwLock<Storage>,
    frame_count: usize,
}

static SIDECAR_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SIDECAR_DISK_BYTES: AtomicU64 = AtomicU64::new(0);
static SIDECAR_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

impl AnimationSource {
    pub(super) fn new(frames: Box<[CompressedDecodedFrameData]>) -> Self {
        Self {
            frame_count: frames.len(),
            storage: RwLock::new(Storage::Memory(frames)),
        }
    }

    pub(super) fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// `(RAM bytes, sidecar file bytes)` this source holds, read together.
    pub(super) fn byte_usage(&self) -> (usize, u64) {
        match &*self.storage.read() {
            Storage::Memory(frames) => (
                crate::image_decode::compressed_animation_source_allocation_bytes(frames),
                0,
            ),
            Storage::Disk(sidecar) => (
                std::mem::size_of_val(&*sidecar.frames) + std::mem::size_of::<Sidecar>(),
                sidecar.file_bytes,
            ),
        }
    }

    /// Bytes this source holds in RAM.
    pub(super) fn ram_bytes(&self) -> usize {
        self.byte_usage().0
    }

    /// One frame as raw BGRA with its dimensions. Reads the sidecar when the
    /// source is on disk, so call it off the UI thread except to rebuild a
    /// released hot frame.
    pub(super) fn read_frame(&self, index: usize) -> Option<(Vec<u8>, u32, u32)> {
        match &*self.storage.read() {
            Storage::Memory(frames) => {
                let frame = frames.get(index)?;
                let data = zstd::bulk::decompress(&frame.compressed, frame.raw_len).ok()?;
                (data.len() == frame.raw_len).then_some((data, frame.width, frame.height))
            }
            Storage::Disk(sidecar) => {
                let frame = sidecar.frames.get(index)?;
                let mut compressed = vec![0u8; frame.compressed_len as usize];
                read_exact_at(&sidecar.file, &mut compressed, frame.offset).ok()?;
                let raw_len = frame.raw_len as usize;
                let data = zstd::bulk::decompress(&compressed, raw_len).ok()?;
                (data.len() == raw_len).then_some((data, frame.width, frame.height))
            }
        }
    }

    /// Write the in-RAM frames to a new sidecar. Blocking file IO; the caller
    /// installs the result with [`Self::install_sidecar`].
    pub(super) fn write_sidecar(&self) -> Option<Sidecar> {
        let dir = sidecar_dir()?;
        let storage = self.storage.read();
        let Storage::Memory(frames) = &*storage else {
            return None;
        };
        let path = dir.join(format!(
            "{}-{}.anim",
            std::process::id(),
            SIDECAR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
            options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        }
        let mut file = options.open(&path).ok()?;
        let mut table = Vec::with_capacity(frames.len());
        let mut offset = 0u64;
        let written = (|| -> std::io::Result<()> {
            let mut writer = std::io::BufWriter::with_capacity(256 * 1024, &mut file);
            for frame in frames.iter() {
                writer.write_all(&frame.compressed)?;
                table.push(SidecarFrame {
                    offset,
                    compressed_len: u32::try_from(frame.compressed.len())
                        .map_err(|_| std::io::ErrorKind::InvalidData)?,
                    raw_len: u32::try_from(frame.raw_len)
                        .map_err(|_| std::io::ErrorKind::InvalidData)?,
                    width: frame.width,
                    height: frame.height,
                });
                offset += frame.compressed.len() as u64;
            }
            writer.flush()
        })();
        if written.is_err() {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return None;
        }
        SIDECAR_DISK_BYTES.fetch_add(offset, Ordering::Relaxed);
        Some(Sidecar {
            file,
            path,
            frames: table.into_boxed_slice(),
            file_bytes: offset,
        })
    }

    /// Switch to reading from `sidecar`, freeing the in-RAM frames.
    pub(super) fn install_sidecar(&self, sidecar: Sidecar) {
        let mut storage = self.storage.write();
        if matches!(&*storage, Storage::Memory(_)) {
            *storage = Storage::Disk(sidecar);
        }
    }
}

/// The session's sidecar directory under the media cache, emptied the first
/// time it is used. `None` when it cannot be created; sources then stay in RAM.
fn sidecar_dir() -> Option<&'static Path> {
    SIDECAR_DIR
        .get_or_init(|| {
            let dir = super::loader::media_cache_base_path().join("anim");
            // Files left by an earlier session. Another running instance keeps
            // reading its own files through open handles even if removed here.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).ok().map(|()| dir)
        })
        .as_deref()
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let read = file.seek_read(buf, offset)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[read..];
        offset += read as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(not(any(windows, unix)))]
fn read_exact_at(_file: &File, _buf: &mut [u8], _offset: u64) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}
