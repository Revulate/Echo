//! Frozen Bincode-v1 mirrors of the disk index types.
//!
//! Kept apart from the current Serde/Postcard models so schema changes there
//! cannot silently redefine how legacy cache bytes decode. They exist only to
//! read, and in tests to write, the retired Bincode index files for one-way
//! migration.

use bincode::Decode;
#[cfg(test)]
use bincode::Encode;

use super::{
    CacheEntry, EntryType, SerializableIndex, SerializableSourceIndex, SourceAlias,
    SourceBlobEncoding, SourceBlobEntry,
};

#[derive(Debug, Decode)]
#[cfg_attr(test, derive(Encode))]
pub(super) struct LegacySerializableIndex {
    version: u32,
    entries: Vec<(String, LegacyCacheEntry)>,
    total_size_bytes: u64,
}

#[derive(Debug, Clone, Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacyCacheEntry {
    hash: String,
    entry_type: LegacyEntryType,
    size_bytes: u64,
    last_accessed: u64,
    access_count: u32,
    width: u32,
    height: u32,
    frame_count: u16,
    total_duration_ms: u32,
}

#[derive(Debug, Clone, Copy, Decode)]
#[cfg_attr(test, derive(Encode))]
enum LegacyEntryType {
    Static,
    Animated,
}

#[derive(Debug, Decode)]
#[cfg_attr(test, derive(Encode))]
pub(super) struct LegacySerializableSourceIndex {
    version: u32,
    identity_aliases: Vec<(String, LegacySourceAlias)>,
    url_aliases: Vec<(String, LegacySourceAlias)>,
    blobs: Vec<(String, LegacySourceBlobEntry)>,
    total_size_bytes: u64,
}

#[derive(Debug, Clone, Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacySourceAlias {
    blob_hash: String,
    last_accessed: u64,
    access_count: u32,
}

#[derive(Debug, Clone, Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacySourceBlobEntry {
    hash: String,
    encoding: LegacySourceBlobEncoding,
    size_bytes: u64,
    original_size_bytes: u64,
    last_accessed: u64,
    access_count: u32,
    width: u32,
    height: u32,
    frame_count: u16,
    total_duration_ms: u32,
}

#[derive(Debug, Clone, Copy, Decode)]
#[cfg_attr(test, derive(Encode))]
enum LegacySourceBlobEncoding {
    Raw,
    Zstd,
}

impl From<LegacyEntryType> for EntryType {
    fn from(value: LegacyEntryType) -> Self {
        match value {
            LegacyEntryType::Static => Self::Static,
            LegacyEntryType::Animated => Self::Animated,
        }
    }
}

#[cfg(test)]
impl From<EntryType> for LegacyEntryType {
    fn from(value: EntryType) -> Self {
        match value {
            EntryType::Static => Self::Static,
            EntryType::Animated => Self::Animated,
        }
    }
}

impl From<LegacyCacheEntry> for CacheEntry {
    fn from(value: LegacyCacheEntry) -> Self {
        Self {
            hash: value.hash,
            entry_type: value.entry_type.into(),
            size_bytes: value.size_bytes,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
            width: value.width,
            height: value.height,
            frame_count: value.frame_count,
            total_duration_ms: value.total_duration_ms,
        }
    }
}

#[cfg(test)]
impl From<CacheEntry> for LegacyCacheEntry {
    fn from(value: CacheEntry) -> Self {
        Self {
            hash: value.hash,
            entry_type: value.entry_type.into(),
            size_bytes: value.size_bytes,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
            width: value.width,
            height: value.height,
            frame_count: value.frame_count,
            total_duration_ms: value.total_duration_ms,
        }
    }
}

impl From<LegacySerializableIndex> for SerializableIndex {
    fn from(value: LegacySerializableIndex) -> Self {
        Self {
            version: value.version,
            entries: value
                .entries
                .into_iter()
                .map(|(url, entry)| (url, entry.into()))
                .collect(),
            total_size_bytes: value.total_size_bytes,
        }
    }
}

#[cfg(test)]
impl From<SerializableIndex> for LegacySerializableIndex {
    fn from(value: SerializableIndex) -> Self {
        Self {
            version: value.version,
            entries: value
                .entries
                .into_iter()
                .map(|(url, entry)| (url, entry.into()))
                .collect(),
            total_size_bytes: value.total_size_bytes,
        }
    }
}

impl From<LegacySourceBlobEncoding> for SourceBlobEncoding {
    fn from(value: LegacySourceBlobEncoding) -> Self {
        match value {
            LegacySourceBlobEncoding::Raw => Self::Raw,
            LegacySourceBlobEncoding::Zstd => Self::Zstd,
        }
    }
}

#[cfg(test)]
impl From<SourceBlobEncoding> for LegacySourceBlobEncoding {
    fn from(value: SourceBlobEncoding) -> Self {
        match value {
            SourceBlobEncoding::Raw => Self::Raw,
            SourceBlobEncoding::Zstd => Self::Zstd,
        }
    }
}

impl From<LegacySourceAlias> for SourceAlias {
    fn from(value: LegacySourceAlias) -> Self {
        Self {
            blob_hash: value.blob_hash,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
        }
    }
}

#[cfg(test)]
impl From<SourceAlias> for LegacySourceAlias {
    fn from(value: SourceAlias) -> Self {
        Self {
            blob_hash: value.blob_hash,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
        }
    }
}

impl From<LegacySourceBlobEntry> for SourceBlobEntry {
    fn from(value: LegacySourceBlobEntry) -> Self {
        Self {
            hash: value.hash,
            encoding: value.encoding.into(),
            size_bytes: value.size_bytes,
            original_size_bytes: value.original_size_bytes,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
            width: value.width,
            height: value.height,
            frame_count: value.frame_count,
            total_duration_ms: value.total_duration_ms,
        }
    }
}

#[cfg(test)]
impl From<SourceBlobEntry> for LegacySourceBlobEntry {
    fn from(value: SourceBlobEntry) -> Self {
        Self {
            hash: value.hash,
            encoding: value.encoding.into(),
            size_bytes: value.size_bytes,
            original_size_bytes: value.original_size_bytes,
            last_accessed: value.last_accessed,
            access_count: value.access_count,
            width: value.width,
            height: value.height,
            frame_count: value.frame_count,
            total_duration_ms: value.total_duration_ms,
        }
    }
}

impl From<LegacySerializableSourceIndex> for SerializableSourceIndex {
    fn from(value: LegacySerializableSourceIndex) -> Self {
        Self {
            version: value.version,
            identity_aliases: value
                .identity_aliases
                .into_iter()
                .map(|(key, alias)| (key, alias.into()))
                .collect(),
            url_aliases: value
                .url_aliases
                .into_iter()
                .map(|(url, alias)| (url, alias.into()))
                .collect(),
            blobs: value
                .blobs
                .into_iter()
                .map(|(hash, entry)| (hash, entry.into()))
                .collect(),
            total_size_bytes: value.total_size_bytes,
        }
    }
}

#[cfg(test)]
impl From<SerializableSourceIndex> for LegacySerializableSourceIndex {
    fn from(value: SerializableSourceIndex) -> Self {
        Self {
            version: value.version,
            identity_aliases: value
                .identity_aliases
                .into_iter()
                .map(|(key, alias)| (key, alias.into()))
                .collect(),
            url_aliases: value
                .url_aliases
                .into_iter()
                .map(|(url, alias)| (url, alias.into()))
                .collect(),
            blobs: value
                .blobs
                .into_iter()
                .map(|(hash, entry)| (hash, entry.into()))
                .collect(),
            total_size_bytes: value.total_size_bytes,
        }
    }
}
