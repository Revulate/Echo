use crate::{Emote, EmoteProvider};
use std::collections::HashMap;
use std::sync::Arc;

/// Category of emotes for picker UI
/// Uses Arc<Emote> to avoid cloning emote data when building categories
#[derive(Clone, Debug)]
pub struct EmoteCategory {
    pub emotes: Vec<Arc<Emote>>,
}

impl EmoteCategory {
    pub fn new(_id: &str, _name: &str, _provider: Option<EmoteProvider>) -> Self {
        Self { emotes: Vec::new() }
    }

    pub fn with_emotes(mut self, emotes: Vec<Arc<Emote>>) -> Self {
        self.emotes = emotes;
        self
    }
}

/// Group emotes into categories for the emote picker
/// Accepts Arc<Emote> references to avoid cloning emote data
pub fn group_emotes_into_categories(
    emotes: Vec<Arc<Emote>>,
    channel_login: &str,
) -> Vec<EmoteCategory> {
    let input_count = emotes.len();

    // Preserve the established provider distribution for large snapshots, but
    // do not reserve the full large-channel budget for every small/medium
    // rebuild. Across these vectors the scaled hints reserve approximately no
    // more slots than the snapshot can actually distribute.
    const CATEGORY_CAPACITY_BUDGET: usize = 3_450;
    #[inline]
    fn category_capacity(input_count: usize, historical_hint: usize) -> usize {
        if input_count >= CATEGORY_CAPACITY_BUDGET {
            return historical_hint;
        }
        input_count
            .saturating_mul(historical_hint)
            .div_ceil(CATEGORY_CAPACITY_BUDGET)
    }

    let mut categories: Vec<EmoteCategory> = Vec::with_capacity(12);

    // Group by channel/provider - uses Arc<Emote> for zero-copy categorization
    let mut sub_emotes: HashMap<String, Vec<Arc<Emote>>> =
        HashMap::with_capacity(30.min(input_count));
    let mut channel_7tv = Vec::with_capacity(category_capacity(input_count, 800));
    let mut channel_bttv = Vec::with_capacity(category_capacity(input_count, 100));
    let mut channel_ffz = Vec::with_capacity(category_capacity(input_count, 100));
    let mut global_7tv = Vec::with_capacity(category_capacity(input_count, 500));
    let mut global_bttv = Vec::with_capacity(category_capacity(input_count, 150));
    let mut global_ffz = Vec::with_capacity(category_capacity(input_count, 100));
    let mut global_twitch = Vec::with_capacity(category_capacity(input_count, 1_500));
    let mut personal_7tv = Vec::with_capacity(category_capacity(input_count, 200));

    #[inline]
    fn push_subscription_emote(
        subscriptions: &mut HashMap<String, Vec<Arc<Emote>>>,
        channel: &str,
        emote: Arc<Emote>,
    ) {
        if let Some(existing) = subscriptions.get_mut(channel) {
            existing.push(emote);
        } else {
            subscriptions.insert(channel.to_string(), vec![emote]);
        }
    }

    for emote in emotes {
        match &emote.channel {
            Some(ch) if ch.starts_with("sub:") => {
                // Subscription emote (Twitch)
                let channel_name = ch.strip_prefix("sub:").unwrap_or(ch);
                if let Some(existing) = sub_emotes.get_mut(channel_name) {
                    existing.push(emote);
                } else {
                    let channel_name = channel_name.to_string();
                    sub_emotes.insert(channel_name, vec![emote]);
                }
            }
            Some(ch) if ch.as_ref() == "personal:7tv" => {
                personal_7tv.push(emote);
            }
            Some(ch) if ch.eq_ignore_ascii_case(channel_login) => {
                // Channel-specific emote
                match emote.provider {
                    EmoteProvider::SevenTV => channel_7tv.push(emote),
                    EmoteProvider::BTTV => channel_bttv.push(emote),
                    EmoteProvider::FFZ => channel_ffz.push(emote),
                    EmoteProvider::Twitch => {
                        // This is a channel sub emote from the channel itself
                        push_subscription_emote(&mut sub_emotes, channel_login, emote);
                    }
                    EmoteProvider::Emoji => {} // Emojis don't belong to channels
                }
            }
            None => {
                // Global emote
                match emote.provider {
                    EmoteProvider::SevenTV => global_7tv.push(emote),
                    EmoteProvider::BTTV => global_bttv.push(emote),
                    EmoteProvider::FFZ => global_ffz.push(emote),
                    EmoteProvider::Twitch => global_twitch.push(emote),
                    EmoteProvider::Emoji => {} // Emojis are handled separately
                }
            }
            Some(_) => {
                // Other channel emotes (shouldn't happen normally)
                match emote.provider {
                    EmoteProvider::SevenTV => channel_7tv.push(emote),
                    EmoteProvider::BTTV => channel_bttv.push(emote),
                    EmoteProvider::FFZ => channel_ffz.push(emote),
                    EmoteProvider::Twitch => {
                        push_subscription_emote(&mut sub_emotes, "other", emote);
                    }
                    EmoteProvider::Emoji => {} // Emojis don't belong to channels
                }
            }
        }
    }

    // Build categories in display order

    // 1. Personal 7TV emotes
    if !personal_7tv.is_empty() {
        categories.push(EmoteCategory {
            emotes: personal_7tv,
        });
    }

    // 2. Subscription emotes (grouped by channel)
    let mut sub_sections: Vec<_> = sub_emotes.into_iter().collect();
    // Preserve case-insensitive ordering without allocating two lowercase
    // strings for every comparator call during category snapshot rebuilds.
    sub_sections.sort_by_cached_key(|(channel, _)| channel.to_ascii_lowercase());

    for (_, emotes) in sub_sections {
        if !emotes.is_empty() {
            categories.push(EmoteCategory { emotes });
        }
    }

    // 3. Channel-specific emotes (7TV has highest priority in display)
    if !channel_7tv.is_empty() {
        categories.push(EmoteCategory {
            emotes: channel_7tv,
        });
    }
    if !channel_bttv.is_empty() {
        categories.push(EmoteCategory {
            emotes: channel_bttv,
        });
    }
    if !channel_ffz.is_empty() {
        categories.push(EmoteCategory {
            emotes: channel_ffz,
        });
    }

    // 4. Global emotes
    if !global_7tv.is_empty() {
        categories.push(EmoteCategory { emotes: global_7tv });
    }
    if !global_bttv.is_empty() {
        categories.push(EmoteCategory {
            emotes: global_bttv,
        });
    }
    if !global_ffz.is_empty() {
        categories.push(EmoteCategory { emotes: global_ffz });
    }
    if !global_twitch.is_empty() {
        categories.push(EmoteCategory {
            emotes: global_twitch,
        });
    }

    categories
}
