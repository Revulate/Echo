//! Application adapters for the GPUI-free slot parser engine.

use std::sync::Arc;

use echo_core::slot::MessageSlot;
use echo_core::slot_parser::{SlotEmoteResolver, TwitchEmoteMap};

use crate::backend::emotes::{
    Emote, EmoteLookupSnapshot, EmoteSender, get_emote_fast_case_sensitive_from_snapshot,
    normalize_channel_key, schedule_channel_refresh_on_unresolved, snapshot_emote_lookups,
    snapshot_emote_lookups_for_sender,
};

pub use echo_core::slot_parser::{parse_emotes_tag, parse_emotes_tag_with_offset};

/// Parse Twitch's annotated GIF spans while retaining normal emote/text parsing
/// for the rest of the message. GIF URLs are supplied by Twitch, not inferred
/// from text links.
pub fn parse_to_slots_with_gifs(
    text: &str,
    channel: &str,
    sender: EmoteSender<'_>,
    twitch_emotes: &TwitchEmoteMap,
    gifs_tag: Option<&str>,
    source_offset: usize,
) -> Vec<MessageSlot> {
    echo_core::twitch_gif::parse_to_slots_with_gifs(
        text,
        twitch_emotes,
        &AppEmoteResolver::capture_for_sender(channel, sender),
        gifs_tag,
        source_offset,
    )
}

struct AppEmoteResolver {
    channel_key: String,
    lookups: EmoteLookupSnapshot,
    callbacks_enabled: bool,
}

impl AppEmoteResolver {
    fn capture(channel: &str, callbacks_enabled: bool) -> Self {
        let channel_key = normalize_channel_key(channel);
        let lookups = snapshot_emote_lookups(&channel_key);
        Self {
            channel_key,
            lookups,
            callbacks_enabled,
        }
    }

    fn capture_for_sender(channel: &str, sender: EmoteSender<'_>) -> Self {
        let channel_key = normalize_channel_key(channel);
        let lookups = snapshot_emote_lookups_for_sender(&channel_key, sender);
        Self {
            channel_key,
            lookups,
            callbacks_enabled: true,
        }
    }
}

impl SlotEmoteResolver for AppEmoteResolver {
    fn resolve(&self, name: &str) -> Option<Arc<Emote>> {
        get_emote_fast_case_sensitive_from_snapshot(&self.lookups, name)
    }

    fn on_resolved(&self, name: &str, emote: &Emote) {
        if !self.callbacks_enabled || !crate::log::emote_trace_enabled() {
            return;
        }
        crate::log_emotes!(
            debug,
            "[PARSE] Word '{}' (ch={}) -> EMOTE HIT: provider={:?}, is_animated={}, url={}",
            name,
            self.channel_key,
            emote.provider,
            emote.is_animated,
            emote.url
        );
    }

    fn on_unresolved(&self, word: &str) {
        if !self.callbacks_enabled {
            return;
        }

        schedule_channel_refresh_on_unresolved(&self.channel_key, word);
        if crate::log::emote_trace_enabled() && word.len() >= 2 && word.len() <= 30 {
            let first = word.chars().next().unwrap_or('a');
            let has_inner_uppercase = word.chars().skip(1).any(char::is_uppercase);
            let has_underscore = word.contains('_');
            if first.is_uppercase() || has_inner_uppercase || has_underscore {
                crate::log_emotes!(
                    debug,
                    "[PARSE] Word '{}' (ch={}) -> TEXT (no emote match, not pending)",
                    word,
                    self.channel_key
                );
            }
        }
    }
}

pub fn parse_to_slots(
    text: &str,
    channel: &str,
    sender: EmoteSender<'_>,
    twitch_emotes: &TwitchEmoteMap,
) -> Vec<MessageSlot> {
    let resolver = AppEmoteResolver::capture_for_sender(channel, sender);
    echo_core::slot_parser::parse_to_slots(text, twitch_emotes, &resolver)
}

pub(crate) fn resolve_input_emote_token(channel_key: &str, word: &str) -> Option<Arc<Emote>> {
    let resolver = AppEmoteResolver::capture(channel_key, false);
    echo_core::slot_parser::resolve_input_emote_token(&resolver, word)
}
