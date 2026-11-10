use echo_emote_core::Emote;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum MessageSlot {
    Text(Arc<str>),
    Emote(Arc<Emote>),
    TwitchEmote {
        id: Arc<str>,
        name: Arc<str>,
    },
    /// A media item identified by Twitch's authoritative IRC `gifs` tag.
    ///
    /// `url` is deliberately the original, complete tag value. Twitch requires
    /// clients to render that URL without synthesizing a CDN derivative.
    TwitchGif(Arc<TwitchGifData>),
    Url {
        url: Arc<str>,
        embed_id: u32,
    },
    Mention {
        username: Arc<str>,
    },
    Emoji {
        emoji: Arc<str>,
    },
}

/// Rare GIF payloads are indirect so ordinary message slots keep their size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwitchGifData {
    pub id: Arc<str>,
    pub url: Arc<str>,
    pub alt_text: Arc<str>,
}

impl MessageSlot {
    pub fn text(value: impl Into<Arc<str>>) -> Self {
        Self::Text(value.into())
    }

    /// What the sender typed for an emote slot. A Twitch tag emote keeps its
    /// message text (`:-)`), which can differ from the resolved emote's name.
    pub fn emote_source_text(&self) -> Option<&str> {
        match self {
            Self::Emote(emote) => Some(&emote.name),
            Self::TwitchEmote { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn emote(emote: Arc<Emote>) -> Self {
        Self::Emote(emote)
    }

    pub fn twitch_emote(name: impl Into<Arc<str>>, id: impl Into<Arc<str>>) -> Self {
        Self::TwitchEmote {
            id: id.into(),
            name: name.into(),
        }
    }

    pub fn twitch_gif(
        id: impl Into<Arc<str>>,
        url: impl Into<Arc<str>>,
        alt_text: impl Into<Arc<str>>,
    ) -> Self {
        Self::TwitchGif(Arc::new(TwitchGifData {
            id: id.into(),
            url: url.into(),
            alt_text: alt_text.into(),
        }))
    }

    pub fn mention(username: impl Into<Arc<str>>) -> Self {
        Self::Mention {
            username: username.into(),
        }
    }

    pub fn emoji(emoji: impl Into<Arc<str>>) -> Self {
        Self::Emoji {
            emoji: emoji.into(),
        }
    }

    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Emote(emote) => &emote.name,
            Self::TwitchEmote { name, .. } => name,
            Self::TwitchGif(gif) => &gif.alt_text,
            Self::Url { url, .. } => url,
            Self::Mention { username } => username,
            Self::Emoji { emoji } => emoji,
        }
    }
}

pub fn compute_structure_hash(slots: &[MessageSlot]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    for slot in slots {
        match slot {
            MessageSlot::Text(text) => {
                0u8.hash(&mut hasher);
                text.hash(&mut hasher);
            }
            MessageSlot::Emote(emote) => {
                1u8.hash(&mut hasher);
                emote.provider.hash(&mut hasher);
                emote.id.hash(&mut hasher);
                emote.name.hash(&mut hasher);
                emote.url.hash(&mut hasher);
                emote.is_animated.hash(&mut hasher);
                emote.zero_width.hash(&mut hasher);
                emote.width.hash(&mut hasher);
                emote.height.hash(&mut hasher);
            }
            MessageSlot::TwitchEmote { id, name } => {
                2u8.hash(&mut hasher);
                id.hash(&mut hasher);
                name.hash(&mut hasher);
            }
            MessageSlot::Url { url, embed_id } => {
                3u8.hash(&mut hasher);
                url.hash(&mut hasher);
                embed_id.hash(&mut hasher);
            }
            MessageSlot::TwitchGif(gif) => {
                6u8.hash(&mut hasher);
                gif.id.hash(&mut hasher);
                gif.url.hash(&mut hasher);
                gif.alt_text.hash(&mut hasher);
            }
            MessageSlot::Mention { username } => {
                4u8.hash(&mut hasher);
                username.hash(&mut hasher);
            }
            MessageSlot::Emoji { emoji } => {
                5u8.hash(&mut hasher);
                emoji.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

pub fn slots_semantically_equal(left: &[MessageSlot], right: &[MessageSlot]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (MessageSlot::Text(left), MessageSlot::Text(right)) => left == right,
                (MessageSlot::Emote(left), MessageSlot::Emote(right)) => {
                    Arc::ptr_eq(left, right) || left.as_ref() == right.as_ref()
                }
                (
                    MessageSlot::TwitchEmote {
                        id: left_id,
                        name: left_name,
                    },
                    MessageSlot::TwitchEmote {
                        id: right_id,
                        name: right_name,
                    },
                ) => left_id == right_id && left_name == right_name,
                (MessageSlot::TwitchGif(left), MessageSlot::TwitchGif(right)) => left == right,
                (
                    MessageSlot::Url {
                        url: left_url,
                        embed_id: left_embed,
                    },
                    MessageSlot::Url {
                        url: right_url,
                        embed_id: right_embed,
                    },
                ) => left_url == right_url && left_embed == right_embed,
                (
                    MessageSlot::Mention {
                        username: left_username,
                    },
                    MessageSlot::Mention {
                        username: right_username,
                    },
                ) => left_username == right_username,
                (MessageSlot::Emoji { emoji: left }, MessageSlot::Emoji { emoji: right }) => {
                    left == right
                }
                _ => false,
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_emote_core::EmoteProvider;

    fn make_test_emote(name: &str, url: &str) -> Arc<Emote> {
        Arc::new(Emote {
            id: Arc::from("test_id"),
            name: Arc::from(name),
            url: Arc::from(url),
            provider: EmoteProvider::SevenTV,
            channel: None,
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: None,
            url_2x: None,
            url_4x: None,
            zero_width: false,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    #[test]
    fn test_slot_creation() {
        let text_slot = MessageSlot::text("hello");
        assert_eq!(text_slot.as_text(), "hello");
        let emote_slot =
            MessageSlot::emote(make_test_emote("Kappa", "https://example.com/kappa.png"));
        assert_eq!(emote_slot.as_text(), "Kappa");
        let twitch_slot = MessageSlot::twitch_emote("Kappa", "25");
        assert_eq!(twitch_slot.as_text(), "Kappa");
    }

    #[test]
    fn test_structure_hash_stability() {
        let slots1 = vec![
            MessageSlot::text("hello "),
            MessageSlot::emote(make_test_emote("Kappa", "https://example.com/kappa.png")),
        ];
        let slots2 = vec![
            MessageSlot::text("hello "),
            MessageSlot::emote(make_test_emote("Kappa", "https://example.com/kappa.png")),
        ];
        assert_eq!(
            compute_structure_hash(&slots1),
            compute_structure_hash(&slots2),
            "Same structure should have same hash"
        );
    }

    #[test]
    fn test_structure_hash_changes() {
        let slots1 = vec![
            MessageSlot::text("hello "),
            MessageSlot::emote(make_test_emote("Kappa", "https://example.com/kappa.png")),
        ];
        let slots2 = vec![
            MessageSlot::text("hello "),
            MessageSlot::emote(make_test_emote(
                "PogChamp",
                "https://example.com/pogchamp.png",
            )),
        ];
        assert_ne!(
            compute_structure_hash(&slots1),
            compute_structure_hash(&slots2),
            "Different emotes should have different hash"
        );
    }

    #[test]
    fn semantic_identity_distinguishes_visually_equal_provider_emotes() {
        let first = make_test_emote("Same", "https://example.com/same.png");
        let mut replacement = first.as_ref().clone();
        replacement.id = Arc::from("replacement-id");
        replacement.provider = EmoteProvider::BTTV;
        let first_slots = vec![MessageSlot::emote(first)];
        let replacement_slots = vec![MessageSlot::emote(Arc::new(replacement))];

        assert!(!slots_semantically_equal(&first_slots, &replacement_slots));
        assert_ne!(
            compute_structure_hash(&first_slots),
            compute_structure_hash(&replacement_slots)
        );
    }
}
