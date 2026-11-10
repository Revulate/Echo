//! Slot resolution layer
//!
//! With immediate resolution, most emotes are resolved at parse time. This layer handles the
//! remaining cases (Twitch emotes from IRC tags) and provides utility functions.

use std::sync::Arc;
use std::sync::OnceLock;

use echo_emote_core::{
    Emote, EmoteProvider, normalize_channel_key, twitch_emote_url, twitch_emote_url_with_scale,
};
use echo_text_core::{EmojiStyle, get_emoji_url};

use crate::slot::MessageSlot;

type TwitchEmoteLookup = fn(&str, &str) -> Option<Arc<Emote>>;

static TWITCH_EMOTE_LOOKUP: OnceLock<TwitchEmoteLookup> = OnceLock::new();
static EMOJI_STYLE: OnceLock<fn() -> EmojiStyle> = OnceLock::new();
static EMOTE_TRACE_ENABLED: OnceLock<fn() -> bool> = OnceLock::new();

/// Install the application's loaded-emote-map lookup.
///
/// Without it every Twitch emote resolves through the id fallback, which is the
/// same path a cache miss already takes.
pub fn install_twitch_emote_lookup(callback: TwitchEmoteLookup) {
    let _ = TWITCH_EMOTE_LOOKUP.set(callback);
}

/// Install the application's configured emoji style.
pub fn install_emoji_style(callback: fn() -> EmojiStyle) {
    let _ = EMOJI_STYLE.set(callback);
}

/// Install the application's `ECHO_EMOTE_TRACE` flag.
pub fn install_emote_trace_enabled(callback: fn() -> bool) {
    let _ = EMOTE_TRACE_ENABLED.set(callback);
}

fn get_twitch_emote_by_id_fast(channel: &str, id: &str) -> Option<Arc<Emote>> {
    TWITCH_EMOTE_LOOKUP.get()?(channel, id)
}

fn emoji_style() -> EmojiStyle {
    EMOJI_STYLE
        .get()
        .map_or_else(EmojiStyle::default, |callback| callback())
}

fn emote_trace_enabled() -> bool {
    EMOTE_TRACE_ENABLED.get().is_some_and(|callback| callback())
}

// ============================================================================
// Version Counter
// ============================================================================

/// Get the current emote resolution version.
/// Delegates to VersionRegistry for centralized version management.
pub fn get_emote_version() -> u64 {
    crate::version_registry::VersionRegistry::emote_resolution_version()
}

// ============================================================================
// Resolved Slot Types
// ============================================================================

/// Result of resolving a slot
#[derive(Clone, Debug)]
pub enum ResolvedSlot {
    /// Plain text
    Text(Arc<str>),

    /// Successfully resolved emote
    Emote(Arc<Emote>),

    /// URL (embeddable)
    Url { url: Arc<str>, embed_id: u32 },

    /// Twitch's inline GIF media. This stays distinct from a URL preview so
    /// it can use the visible-only animated media path.
    TwitchGif {
        id: Arc<str>,
        url: Arc<str>,
        alt_text: Arc<str>,
    },

    /// @mention
    Mention(Arc<str>),

    /// Unicode emoji rendered as image (Twemoji)
    Emoji {
        /// The emoji character(s)
        emoji: Arc<str>,
        /// CDN URL for the emoji image
        url: String,
    },
}

// ============================================================================
// Slot Resolver
// ============================================================================

/// Resolves slots to their actual content
///
/// With immediate resolution, most emotes are already resolved at parse
/// time. This resolver handles Twitch emotes (from IRC tags)
/// and provides a unified interface for rendering.
pub struct SlotResolver {
    _channel: Arc<str>,
}

impl SlotResolver {
    /// Create a new resolver for a channel
    pub fn new(channel: impl Into<Arc<str>>) -> Self {
        let channel: Arc<str> = channel.into();
        if !channel.starts_with('#') && channel.bytes().all(|byte| !byte.is_ascii_uppercase()) {
            return Self { _channel: channel };
        }

        Self {
            _channel: Arc::from(normalize_channel_key(channel.as_ref())),
        }
    }

    /// Resolve a slot to its content
    ///
    /// With immediate resolution, most slots are already resolved:
    /// - Emote slots contain the full emote data
    /// - TwitchEmote slots are converted to Emote on the fly
    pub fn resolve(&self, slot: &MessageSlot) -> ResolvedSlot {
        match slot {
            MessageSlot::Text(text) => ResolvedSlot::Text(text.clone()),

            // Already resolved at parse time - just wrap it
            MessageSlot::Emote(emote) => ResolvedSlot::Emote(emote.clone()),

            // Twitch emotes from IRC tags - prefer loaded map data (correct URL + is_animated)
            MessageSlot::TwitchEmote { id, name } => {
                if let Some(emote) = get_twitch_emote_by_id_fast(&self._channel, id) {
                    if emote_trace_enabled() {
                        crate::log_emotes!(
                            debug,
                            "[RESOLVE] TwitchEmote '{}' (id={}) -> ID HIT: url={}, is_animated={}, provider={:?}",
                            name,
                            id,
                            emote.url,
                            emote.is_animated,
                            emote.provider
                        );
                    }
                    ResolvedSlot::Emote(emote)
                } else {
                    let fallback = create_twitch_emote(id, name);
                    if emote_trace_enabled() {
                        crate::log_emotes!(
                            debug,
                            "[RESOLVE] TwitchEmote '{}' (id={}) -> FALLBACK (ID MISS): url={}, is_animated={}",
                            name,
                            id,
                            fallback.url,
                            fallback.is_animated
                        );
                    }
                    ResolvedSlot::Emote(Arc::new(fallback))
                }
            }

            MessageSlot::Url { url, embed_id } => ResolvedSlot::Url {
                url: url.clone(),
                embed_id: *embed_id,
            },

            MessageSlot::TwitchGif(gif) => ResolvedSlot::TwitchGif {
                id: gif.id.clone(),
                url: gif.url.clone(),
                alt_text: gif.alt_text.clone(),
            },

            MessageSlot::Mention { username } => ResolvedSlot::Mention(username.clone()),

            // Unicode emoji - generate CDN URL for Twemoji rendering
            MessageSlot::Emoji { emoji } => {
                let url = get_emoji_url(emoji, &emoji_style());
                ResolvedSlot::Emoji {
                    emoji: emoji.clone(),
                    url,
                }
            }
        }
    }
}

/// Create a Twitch emote from ID and name
fn create_twitch_emote(id: &Arc<str>, name: &Arc<str>) -> Emote {
    let url_2x: Arc<str> = Arc::from(twitch_emote_url(id));
    Emote {
        id: Arc::clone(id),
        name: Arc::clone(name),
        url: Arc::clone(&url_2x),
        provider: EmoteProvider::Twitch,
        channel: None,
        original_name: None,
        creator_name: None,
        twitch_owner_login: None,
        twitch_owner_id: None,
        twitch_emote_type: None,
        url_1x: Some(Arc::from(twitch_emote_url_with_scale(id, "1.0"))),
        url_2x: Some(url_2x),
        url_4x: Some(Arc::from(twitch_emote_url_with_scale(id, "3.0"))),
        zero_width: false,
        is_animated: true,
        width: Some(echo_emote_core::twitch_base_dimensions(id).0),
        height: Some(echo_emote_core::twitch_base_dimensions(id).1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolver_text() {
        let resolver = SlotResolver::new("test");
        let slot = MessageSlot::text("hello");
        let resolved = resolver.resolve(&slot);

        assert!(matches!(resolved, ResolvedSlot::Text(t) if t.as_ref() == "hello"));
    }

    #[test]
    fn test_resolver_url() {
        let resolver = SlotResolver::new("test");
        let slot = MessageSlot::Url {
            url: Arc::from("https://example.com"),
            embed_id: 1,
        };
        let resolved = resolver.resolve(&slot);

        assert!(
            matches!(resolved, ResolvedSlot::Url { url, embed_id: 1 } if url.as_ref() == "https://example.com")
        );
    }
}
