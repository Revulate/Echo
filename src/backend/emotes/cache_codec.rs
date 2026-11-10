use super::{DISK_CACHE_REFRESH_DELAY_SECS, Emote, EmoteProvider};
use bincode::Decode;
#[cfg(test)]
use bincode::Encode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Legacy Bincode v9 configuration for emote cache migration only.
///
/// The maintained `cu-bincode` fork remains wire-compatible with Echo's old
/// v9 cache. New snapshots use Postcard; this configuration must never be
/// used by a current writer.
/// Uses standard config which includes:
/// - Variable integer encoding (enums as u8 instead of u32 for smaller files)
/// - Little endian (optimal for most desktop platforms)
/// - 50MB size limit for security against memory exhaustion attacks
const LEGACY_BINCODE_CONFIG: bincode::config::Configuration<
    bincode::config::LittleEndian,
    bincode::config::Varint,
    bincode::config::Limit<52_428_800>,
> = bincode::config::standard().with_limit::<52_428_800>();

/// Current Postcard cache schema version - increment when changing the
/// structure or persisted semantics.
/// v2: Changed enum serialization format, added user_emotes caching
/// v3: Fixed bincode incompatibility with skip_serializing_if
/// v4: Upgraded to bincode 2.0 with native Encode/Decode traits for better performance
/// v5: Previous version
/// v6: Added zstd compression for 60-70% smaller disk cache
/// v7: Added is_animated, width, height fields for proper emote rendering
/// v8: Persisted the authenticated Twitch user-emote cache owner
/// v9: Legacy Bincode cache format
/// v10: Postcard payload with an explicit cache magic and codec generation
/// v11: Emote dimensions are logical 1x geometry (not selected source pixels)
/// v12: 7TV emotes keep every alias of a shared emote id
/// v13: Missing provider geometry stays unknown rather than defaulting to 28x28
pub(super) const EMOTE_CACHE_VERSION: u32 = 13;
const LEGACY_BINCODE_EMOTE_CACHE_VERSION: u32 = 9;

/// The on-disk header distinguishes current Postcard snapshots from legacy
/// Bincode data before deserialization. The codec generation provides an
/// explicit migration boundary independent of the schema version inside the
/// payload.
const EMOTE_CACHE_MAGIC: &[u8; 8] = b"ECHOMTE\0";
const EMOTE_CACHE_CODEC_GENERATION: u8 = 1;
#[cfg(test)]
const EMOTE_CACHE_HEADER_LEN: usize = EMOTE_CACHE_MAGIC.len() + 1;

/// Both compressed and decompressed snapshots are bounded at 50 MiB, so a
/// corrupt file cannot make the decoder or zstd expansion allocate beyond the
/// cache budget.
const MAX_EMOTE_CACHE_COMPRESSED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_EMOTE_CACHE_DECOMPRESSED_BYTES: usize = 50 * 1024 * 1024;

/// Persistent emote cache file structure.
///
/// Current snapshots are Postcard payloads behind `EMOTE_CACHE_MAGIC` and
/// `EMOTE_CACHE_CODEC_GENERATION`. Keep its serde representation stable: the
/// Bincode v9 compatibility reader uses a separate native-codec wrapper.
#[derive(Serialize, Deserialize)]
pub(super) struct EmoteCacheFile {
    /// Schema version for future migrations
    pub(super) version: u32,
    /// Global emotes from all providers
    pub(super) global_emotes: Option<CachedEmoteSet>,
    /// User's subscription emotes
    pub(super) user_emotes: Option<CachedEmoteSet>,
    /// Twitch user ID that produced `user_emotes`; unknown caches are never
    /// trusted across authenticated accounts.
    pub(super) user_emote_owner_id: Option<String>,
    /// Per-channel emotes (channel_login -> emotes)
    pub(super) channel_emotes: HashMap<String, CachedEmoteSet>,
}

/// Echo's former Bincode v9 cache schema, retained solely to migrate an
/// existing `emote_cache.bincode` into the current Postcard file. Do not add a
/// writer for this type.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
struct LegacyBincodeEmoteCacheFile {
    version: u32,
    global_emotes: Option<LegacyCachedEmoteSet>,
    user_emotes: Option<LegacyCachedEmoteSet>,
    user_emote_owner_id: Option<String>,
    #[bincode(with_serde)]
    channel_emotes: HashMap<String, LegacyCachedEmoteSet>,
}

impl From<LegacyBincodeEmoteCacheFile> for EmoteCacheFile {
    fn from(legacy: LegacyBincodeEmoteCacheFile) -> Self {
        Self {
            version: EMOTE_CACHE_VERSION,
            global_emotes: legacy.global_emotes.map(Into::into),
            user_emotes: legacy.user_emotes.map(Into::into),
            user_emote_owner_id: legacy.user_emote_owner_id,
            channel_emotes: legacy
                .channel_emotes
                .into_iter()
                .map(|(channel, emotes)| (channel, emotes.into()))
                .collect(),
        }
    }
}

/// Frozen serde representation of the former Bincode v9 emote payload.
///
/// This must evolve independently from the current Postcard model so a future
/// emote-field change cannot silently reinterpret old cache bytes.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct LegacyEmote {
    id: Arc<str>,
    name: Arc<str>,
    url: Arc<str>,
    provider: LegacyEmoteProvider,
    #[serde(default)]
    channel: Option<Arc<str>>,
    #[serde(default)]
    original_name: Option<Arc<str>>,
    #[serde(default)]
    creator_name: Option<Arc<str>>,
    #[serde(default)]
    twitch_owner_login: Option<Arc<str>>,
    #[serde(default)]
    twitch_owner_id: Option<Arc<str>>,
    #[serde(default)]
    twitch_emote_type: Option<Arc<str>>,
    #[serde(default)]
    url_1x: Option<Arc<str>>,
    #[serde(default)]
    url_2x: Option<Arc<str>>,
    #[serde(default)]
    url_4x: Option<Arc<str>>,
    #[serde(default)]
    zero_width: bool,
    #[serde(default)]
    is_animated: bool,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

impl From<LegacyEmote> for Emote {
    fn from(legacy: LegacyEmote) -> Self {
        let provider = legacy.provider.into();
        let (width, height) = migrate_inferred_dimensions(provider, legacy.width, legacy.height);
        Self {
            id: legacy.id,
            name: legacy.name,
            url: legacy.url,
            provider,
            channel: legacy.channel,
            original_name: legacy.original_name,
            creator_name: legacy.creator_name,
            twitch_owner_login: legacy.twitch_owner_login,
            twitch_owner_id: legacy.twitch_owner_id,
            twitch_emote_type: legacy.twitch_emote_type,
            url_1x: legacy.url_1x,
            url_2x: legacy.url_2x,
            url_4x: legacy.url_4x,
            zero_width: legacy.zero_width,
            is_animated: legacy.is_animated,
            width,
            height,
        }
    }
}

fn migrate_inferred_dimensions(
    provider: EmoteProvider,
    width: Option<u32>,
    height: Option<u32>,
) -> (Option<u32>, Option<u32>) {
    if matches!(provider, EmoteProvider::BTTV | EmoteProvider::FFZ)
        && width == Some(28)
        && height == Some(28)
    {
        (None, None)
    } else {
        (width, height)
    }
}

/// Frozen v9 serde enum order. Bincode's serde representation uses enum
/// declaration order, so these variant positions are part of the cache ABI.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[allow(clippy::upper_case_acronyms)]
enum LegacyEmoteProvider {
    Twitch,
    SevenTV,
    BTTV,
    FFZ,
    Emoji,
}

impl From<LegacyEmoteProvider> for EmoteProvider {
    fn from(legacy: LegacyEmoteProvider) -> Self {
        match legacy {
            LegacyEmoteProvider::Twitch => Self::Twitch,
            LegacyEmoteProvider::SevenTV => Self::SevenTV,
            LegacyEmoteProvider::BTTV => Self::BTTV,
            LegacyEmoteProvider::FFZ => Self::FFZ,
            LegacyEmoteProvider::Emoji => Self::Emoji,
        }
    }
}

/// Frozen v9 native Bincode envelope for an emote set. Its map intentionally
/// uses serde because that is how v9 wrote `Emote` values.
#[derive(Decode, Deserialize)]
#[cfg_attr(test, derive(Encode, Serialize))]
struct LegacyCachedEmoteSet {
    saved_at: u64,
    #[bincode(with_serde)]
    emotes: HashMap<String, LegacyEmote>,
}

impl From<LegacyCachedEmoteSet> for CachedEmoteSet {
    fn from(legacy: LegacyCachedEmoteSet) -> Self {
        Self {
            saved_at: legacy.saved_at,
            emotes: legacy
                .emotes
                .into_iter()
                .map(|(name, emote)| (name, emote.into()))
                .collect(),
        }
    }
}

/// A set of emotes with timestamp for TTL checking.
///
/// This is the current Postcard representation and must not participate in
/// Bincode's legacy cache ABI.
#[derive(Serialize, Deserialize)]
pub(super) struct CachedEmoteSet {
    /// Unix timestamp when these emotes were fetched
    pub(super) saved_at: u64,
    /// The emotes (name -> Emote)
    pub(super) emotes: HashMap<String, Emote>,
}

impl CachedEmoteSet {
    pub(super) fn new(emotes: HashMap<String, Emote>) -> Self {
        let saved_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self { saved_at, emotes }
    }

    pub(super) fn is_expired(&self, ttl_secs: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.saved_at) > ttl_secs
    }

    /// Check if this cache entry is fresh enough to delay refresh
    /// Returns true if cache was saved less than DISK_CACHE_REFRESH_DELAY_SECS ago
    pub(super) fn is_fresh_enough_to_delay(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let age_secs = now.saturating_sub(self.saved_at);
        age_secs < DISK_CACHE_REFRESH_DELAY_SECS
    }
}

pub(super) fn legacy_emote_cache_path(current_path: &Path) -> PathBuf {
    current_path.with_extension("bincode")
}

#[derive(Debug)]
pub(super) enum EmoteCacheReadError {
    NotFound,
    Io(String),
    Invalid(String),
}

impl std::fmt::Display for EmoteCacheReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("cache file was not found"),
            Self::Io(message) | Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

pub(super) enum EmoteCacheReadSource {
    Current(EmoteCacheFile),
    Legacy(EmoteCacheFile),
}

pub(super) enum EmoteCacheLoadError {
    Current(EmoteCacheReadError),
    Legacy(EmoteCacheReadError),
}

fn read_bounded_emote_cache_file(path: &Path) -> Result<Vec<u8>, EmoteCacheReadError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(EmoteCacheReadError::NotFound);
        }
        Err(error) => {
            return Err(EmoteCacheReadError::Io(format!(
                "failed to open {}: {error}",
                path.display()
            )));
        }
    };
    let advertised_bytes = file
        .metadata()
        .map_err(|error| {
            EmoteCacheReadError::Io(format!("failed to inspect {}: {error}", path.display()))
        })?
        .len();
    if advertised_bytes > MAX_EMOTE_CACHE_COMPRESSED_BYTES {
        return Err(EmoteCacheReadError::Invalid(format!(
            "compressed cache exceeds {} byte limit ({} bytes)",
            MAX_EMOTE_CACHE_COMPRESSED_BYTES, advertised_bytes
        )));
    }

    let capture_limit = MAX_EMOTE_CACHE_COMPRESSED_BYTES.saturating_add(1);
    let mut bytes = Vec::new();
    file.take(capture_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            EmoteCacheReadError::Io(format!("failed to read {}: {error}", path.display()))
        })?;
    if bytes.len() as u64 > MAX_EMOTE_CACHE_COMPRESSED_BYTES {
        return Err(EmoteCacheReadError::Invalid(format!(
            "compressed cache exceeds {} byte limit while reading",
            MAX_EMOTE_CACHE_COMPRESSED_BYTES
        )));
    }
    Ok(bytes)
}

fn decode_bounded_emote_cache_payload(compressed: &[u8]) -> Result<Vec<u8>, EmoteCacheReadError> {
    let mut decoder =
        zstd::stream::read::Decoder::new(Cursor::new(compressed)).map_err(|error| {
            EmoteCacheReadError::Invalid(format!("failed to create zstd decoder: {error}"))
        })?;
    decoder.window_log_max(25).map_err(|error| {
        EmoteCacheReadError::Invalid(format!("failed to bound zstd window: {error}"))
    })?;

    let capture_limit = u64::try_from(MAX_EMOTE_CACHE_DECOMPRESSED_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bounded_decoder = decoder.take(capture_limit);
    let mut payload = Vec::new();
    bounded_decoder.read_to_end(&mut payload).map_err(|error| {
        EmoteCacheReadError::Invalid(format!("failed to decompress cache: {error}"))
    })?;
    if payload.len() > MAX_EMOTE_CACHE_DECOMPRESSED_BYTES {
        return Err(EmoteCacheReadError::Invalid(format!(
            "decompressed cache exceeds {} byte limit",
            MAX_EMOTE_CACHE_DECOMPRESSED_BYTES
        )));
    }
    Ok(payload)
}

/// A write-through size gate for the uncompressed Postcard stream. It rejects
/// writes before forwarding the overflowing chunk, so the temporary snapshot
/// can never contain more than the persisted cache budget.
struct BoundedCacheWriter<W> {
    inner: W,
    written: usize,
    limit: usize,
}

impl<W> BoundedCacheWriter<W> {
    fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            written: 0,
            limit,
        }
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for BoundedCacheWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.written) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized emote cache exceeds its size limit",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(count);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
fn serialize_current_emote_cache(cache_file: &EmoteCacheFile) -> Result<Vec<u8>, String> {
    let postcard_payload = postcard::to_stdvec(cache_file)
        .map_err(|error| format!("failed to serialize Postcard cache: {error}"))?;
    let mut payload = Vec::with_capacity(EMOTE_CACHE_HEADER_LEN + postcard_payload.len());
    payload.extend_from_slice(EMOTE_CACHE_MAGIC);
    payload.push(EMOTE_CACHE_CODEC_GENERATION);
    payload.extend_from_slice(&postcard_payload);
    if payload.len() > MAX_EMOTE_CACHE_DECOMPRESSED_BYTES {
        return Err(format!(
            "serialized cache exceeds {} byte limit",
            MAX_EMOTE_CACHE_DECOMPRESSED_BYTES
        ));
    }
    Ok(payload)
}

fn decode_current_emote_cache(payload: &[u8]) -> Result<EmoteCacheFile, String> {
    let Some(after_magic) = payload.strip_prefix(EMOTE_CACHE_MAGIC) else {
        return Err("current cache magic is missing or invalid".to_string());
    };
    let Some((&generation, postcard_payload)) = after_magic.split_first() else {
        return Err("current cache codec generation is missing".to_string());
    };
    if generation != EMOTE_CACHE_CODEC_GENERATION {
        return Err(format!(
            "unsupported current cache codec generation {generation}"
        ));
    }

    let (mut cache_file, remaining) = postcard::take_from_bytes::<EmoteCacheFile>(postcard_payload)
        .map_err(|error| format!("failed to deserialize Postcard cache: {error}"))?;
    if !remaining.is_empty() {
        return Err(format!(
            "current cache has {} unexpected trailing bytes",
            remaining.len()
        ));
    }
    if cache_file.version == 12 {
        // Old snapshots cannot distinguish a provider's real square from the
        // manufactured default. Reuse the sets, but refine ambiguous geometry.
        for set in cache_file
            .global_emotes
            .iter_mut()
            .chain(cache_file.user_emotes.iter_mut())
            .chain(cache_file.channel_emotes.values_mut())
        {
            for emote in set.emotes.values_mut() {
                (emote.width, emote.height) =
                    migrate_inferred_dimensions(emote.provider, emote.width, emote.height);
            }
        }
        cache_file.version = EMOTE_CACHE_VERSION;
    } else if cache_file.version != EMOTE_CACHE_VERSION {
        return Err(format!(
            "current cache schema version {} does not match expected {}",
            cache_file.version, EMOTE_CACHE_VERSION
        ));
    }
    Ok(cache_file)
}

fn decode_legacy_bincode_emote_cache(payload: &[u8]) -> Result<EmoteCacheFile, String> {
    let (legacy, consumed) = bincode::decode_from_slice::<LegacyBincodeEmoteCacheFile, _>(
        payload,
        LEGACY_BINCODE_CONFIG,
    )
    .map_err(|error| format!("failed to deserialize legacy Bincode cache: {error}"))?;
    if consumed != payload.len() {
        return Err(format!(
            "legacy cache has {} unexpected trailing bytes",
            payload.len().saturating_sub(consumed)
        ));
    }
    if legacy.version != LEGACY_BINCODE_EMOTE_CACHE_VERSION {
        return Err(format!(
            "legacy cache schema version {} does not match expected {}",
            legacy.version, LEGACY_BINCODE_EMOTE_CACHE_VERSION
        ));
    }
    Ok(legacy.into())
}

fn read_current_emote_cache(path: &Path) -> Result<EmoteCacheFile, EmoteCacheReadError> {
    let compressed = read_bounded_emote_cache_file(path)?;
    let payload = decode_bounded_emote_cache_payload(&compressed)?;
    decode_current_emote_cache(&payload).map_err(EmoteCacheReadError::Invalid)
}

fn read_legacy_bincode_emote_cache(path: &Path) -> Result<EmoteCacheFile, EmoteCacheReadError> {
    let compressed = read_bounded_emote_cache_file(path)?;
    let payload = decode_bounded_emote_cache_payload(&compressed)?;
    decode_legacy_bincode_emote_cache(&payload).map_err(EmoteCacheReadError::Invalid)
}

/// Legacy migration is only allowed when no current cache file exists. A
/// corrupt, unsupported, or inaccessible Postcard file is authoritative
/// evidence and must never be overwritten by stale Bincode data.
pub(super) fn read_emote_cache_with_legacy_fallback(
    current_path: &Path,
    legacy_path: &Path,
) -> Result<EmoteCacheReadSource, EmoteCacheLoadError> {
    match read_current_emote_cache(current_path) {
        Ok(cache_file) => Ok(EmoteCacheReadSource::Current(cache_file)),
        Err(EmoteCacheReadError::NotFound) => read_legacy_bincode_emote_cache(legacy_path)
            .map(EmoteCacheReadSource::Legacy)
            .map_err(EmoteCacheLoadError::Legacy),
        Err(error) => Err(EmoteCacheLoadError::Current(error)),
    }
}

pub(super) fn write_current_emote_cache_atomic(
    path: &Path,
    cache_file: &EmoteCacheFile,
) -> Result<u64, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create cache directory: {error}"))?;
    }
    let unique_id = format!(
        "{}.{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    );
    let tmp_path = path.with_extension(format!("tmp.{unique_id}"));
    let write_result = (|| {
        let file = std::fs::File::create(&tmp_path)
            .map_err(|error| format!("failed to create temp cache file: {error}"))?;
        let encoder = zstd::stream::Encoder::new(BufWriter::new(file), 3)
            .map_err(|error| format!("failed to create zstd encoder: {error}"))?;
        let mut bounded_encoder =
            BoundedCacheWriter::new(encoder, MAX_EMOTE_CACHE_DECOMPRESSED_BYTES);
        bounded_encoder
            .write_all(EMOTE_CACHE_MAGIC)
            .and_then(|()| bounded_encoder.write_all(&[EMOTE_CACHE_CODEC_GENERATION]))
            .map_err(|error| format!("failed to write Postcard cache header: {error}"))?;
        let bounded_encoder = postcard::to_io(cache_file, bounded_encoder)
            .map_err(|error| format!("failed to serialize Postcard cache: {error}"))?;
        let encoder = bounded_encoder.into_inner();
        let mut writer = encoder
            .finish()
            .map_err(|error| format!("failed to finish zstd compression: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("failed to flush temp cache file: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("failed to sync temp cache file: {error}"))?;
        drop(writer);

        let compressed_bytes = std::fs::metadata(&tmp_path)
            .map_err(|error| format!("failed to inspect temp cache file: {error}"))?
            .len();
        if compressed_bytes > MAX_EMOTE_CACHE_COMPRESSED_BYTES {
            return Err(format!(
                "compressed cache exceeds {} byte limit",
                MAX_EMOTE_CACHE_COMPRESSED_BYTES
            ));
        }
        crate::utils::replace_file(&tmp_path, path)
            .map_err(|error| format!("failed to replace cache file: {error}"))?;
        Ok::<_, String>(compressed_bytes)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result
}

#[cfg(test)]
mod emote_cache_codec_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn old_placeholder_dimensions_become_unknown_without_changing_real_geometry() {
        for provider in [EmoteProvider::BTTV, EmoteProvider::FFZ] {
            assert_eq!(
                migrate_inferred_dimensions(provider, Some(28), Some(28)),
                (None, None)
            );
            assert_eq!(
                migrate_inferred_dimensions(provider, Some(56), Some(28)),
                (Some(56), Some(28))
            );
        }
        assert_eq!(
            migrate_inferred_dimensions(EmoteProvider::Twitch, Some(28), Some(28)),
            (Some(28), Some(28))
        );
    }

    #[test]
    fn previous_postcard_schema_is_migrated_without_discarding_the_snapshot() {
        let mut cache = empty_current_cache();
        cache.version = 12;
        let payload = serialize_current_emote_cache(&cache).unwrap();
        assert_eq!(
            decode_current_emote_cache(&payload).unwrap().version,
            EMOTE_CACHE_VERSION
        );
    }

    fn empty_current_cache() -> EmoteCacheFile {
        EmoteCacheFile {
            version: EMOTE_CACHE_VERSION,
            global_emotes: None,
            user_emotes: None,
            user_emote_owner_id: None,
            channel_emotes: HashMap::new(),
        }
    }

    fn empty_legacy_cache() -> LegacyBincodeEmoteCacheFile {
        LegacyBincodeEmoteCacheFile {
            version: LEGACY_BINCODE_EMOTE_CACHE_VERSION,
            global_emotes: None,
            user_emotes: None,
            user_emote_owner_id: None,
            channel_emotes: HashMap::new(),
        }
    }

    #[test]
    fn postcard_cache_has_explicit_header_and_exact_payload_consumption() {
        let mut payload = serialize_current_emote_cache(&empty_current_cache())
            .expect("empty cache should serialize");
        assert!(payload.starts_with(EMOTE_CACHE_MAGIC));
        assert_eq!(
            payload[EMOTE_CACHE_MAGIC.len()],
            EMOTE_CACHE_CODEC_GENERATION
        );
        assert_eq!(
            decode_current_emote_cache(&payload)
                .expect("complete payload should deserialize")
                .version,
            EMOTE_CACHE_VERSION
        );

        payload.push(0);
        assert!(
            decode_current_emote_cache(&payload)
                .err()
                .expect("trailing data must be rejected")
                .contains("trailing")
        );
    }

    #[test]
    fn postcard_cache_rejects_unknown_codec_generation() {
        let mut payload = serialize_current_emote_cache(&empty_current_cache())
            .expect("empty cache should serialize");
        payload[EMOTE_CACHE_MAGIC.len()] = EMOTE_CACHE_CODEC_GENERATION.saturating_add(1);
        assert!(
            decode_current_emote_cache(&payload)
                .err()
                .expect("unknown codec generations must not deserialize")
                .contains("codec generation")
        );
    }

    #[test]
    fn legacy_bincode_v9_reader_remains_exact_and_read_only() {
        let mut payload = bincode::encode_to_vec(empty_legacy_cache(), LEGACY_BINCODE_CONFIG)
            .expect("legacy fixture should encode");
        assert_eq!(
            decode_legacy_bincode_emote_cache(&payload)
                .expect("legacy v9 fixture should decode")
                .version,
            EMOTE_CACHE_VERSION
        );

        payload.push(0);
        assert!(
            decode_legacy_bincode_emote_cache(&payload)
                .err()
                .expect("legacy payload trailing data must be rejected")
                .contains("trailing")
        );
    }

    #[test]
    fn legacy_path_replaces_only_the_current_extension() {
        assert_eq!(
            legacy_emote_cache_path(Path::new("cache/emote_cache.postcard")),
            PathBuf::from("cache/emote_cache.bincode")
        );
    }

    #[test]
    fn corrupt_current_cache_never_falls_back_or_overwrites_with_valid_legacy() {
        let temp_root = std::env::temp_dir().join(format!(
            "echo-emote-cache-fallback-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).expect("temporary cache directory should exist");
        let current_path = temp_root.join("emote_cache.postcard");
        let legacy_path = legacy_emote_cache_path(&current_path);
        let corrupt_current = b"not a zstd Postcard cache".to_vec();
        std::fs::write(&current_path, &corrupt_current)
            .expect("corrupt current fixture should write");

        let legacy_payload = bincode::encode_to_vec(empty_legacy_cache(), LEGACY_BINCODE_CONFIG)
            .expect("legacy fixture should encode");
        let legacy_compressed = zstd::stream::encode_all(Cursor::new(legacy_payload), 3)
            .expect("legacy fixture should compress");
        std::fs::write(&legacy_path, legacy_compressed).expect("legacy fixture should write");

        assert!(matches!(
            read_emote_cache_with_legacy_fallback(&current_path, &legacy_path),
            Err(EmoteCacheLoadError::Current(EmoteCacheReadError::Invalid(
                _
            )))
        ));
        assert_eq!(
            std::fs::read(&current_path).expect("current cache should remain readable"),
            corrupt_current,
            "a corrupt current cache is evidence and must not be overwritten by legacy data"
        );
        std::fs::remove_dir_all(&temp_root).expect("temporary cache directory should clean up");
    }

    #[test]
    fn missing_current_cache_reports_corrupt_legacy_as_a_legacy_failure() {
        let temp_root = std::env::temp_dir().join(format!(
            "echo-emote-cache-legacy-failure-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).expect("temporary cache directory should exist");
        let current_path = temp_root.join("emote_cache.postcard");
        let legacy_path = legacy_emote_cache_path(&current_path);
        std::fs::write(&legacy_path, b"not a zstd Bincode cache")
            .expect("corrupt legacy fixture should write");

        assert!(matches!(
            read_emote_cache_with_legacy_fallback(&current_path, &legacy_path),
            Err(EmoteCacheLoadError::Legacy(EmoteCacheReadError::Invalid(_)))
        ));
        assert!(
            !current_path.exists(),
            "a corrupt legacy cache must not create a current Postcard snapshot"
        );
        std::fs::remove_dir_all(&temp_root).expect("temporary cache directory should clean up");
    }
}
