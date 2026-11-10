//! Render-unit grouping for resolved message slots.
//!
//! This keeps zero-width emote layering explicit before layout. The parser still
//! stores individual slots, but consumers that render or preload media can work
//! with a logical base-emote-plus-overlays unit instead of rediscovering that
//! relationship locally.

use smallvec::SmallVec;
use std::sync::Arc;

use crate::backend::emotes::Emote;

use super::MessageSlot;
use super::resolver::{ResolvedSlot, SlotResolver};

#[derive(Clone, Debug)]
pub struct EmoteRenderUnit {
    pub base: Arc<Emote>,
    pub overlays: SmallVec<[Arc<Emote>; 4]>,
}

impl EmoteRenderUnit {
    fn new(base: Arc<Emote>) -> Self {
        Self {
            base,
            overlays: SmallVec::new(),
        }
    }

    fn add_overlay(&mut self, overlay: Arc<Emote>) {
        self.overlays.push(overlay);
    }

    #[inline]
    pub fn for_each_emote(&self, mut visit: impl FnMut(&Arc<Emote>)) {
        visit(&self.base);
        for overlay in &self.overlays {
            visit(overlay);
        }
    }
}

#[derive(Clone, Debug)]
pub enum SlotRenderUnit {
    Text(Arc<str>),
    Emote(EmoteRenderUnit),
    /// Twitch-authoritative inline media. Kept separate from a link preview.
    TwitchGif {
        id: Arc<str>,
        url: Arc<str>,
        alt_text: Arc<str>,
    },
    Url {
        url: Arc<str>,
        embed_id: u32,
    },
    Mention(Arc<str>),
    Emoji {
        emoji: Arc<str>,
        url: Arc<str>,
    },
}

impl SlotRenderUnit {
    #[inline]
    pub fn is_substantive_content(&self) -> bool {
        matches!(
            self,
            SlotRenderUnit::Text(_)
                | SlotRenderUnit::Emote(_)
                | SlotRenderUnit::TwitchGif { .. }
                | SlotRenderUnit::Url { .. }
                | SlotRenderUnit::Mention(_)
                | SlotRenderUnit::Emoji { .. }
        )
    }

    #[inline]
    pub fn for_each_emote(&self, visit: impl FnMut(&Arc<Emote>)) {
        if let SlotRenderUnit::Emote(unit) = self {
            unit.for_each_emote(visit);
        }
    }

    #[inline]
    pub fn for_each_url(&self, mut visit: impl FnMut(&Arc<str>, u32)) {
        if let SlotRenderUnit::Url { url, embed_id } = self {
            visit(url, *embed_id);
        }
    }
}

pub fn resolve_render_units(
    slots: &[MessageSlot],
    resolver: &SlotResolver,
    zero_width_enabled: bool,
) -> Vec<SlotRenderUnit> {
    group_resolved_slots(
        slots.iter().map(|slot| resolver.resolve(slot)),
        zero_width_enabled,
    )
}

pub fn group_resolved_slots(
    resolved_slots: impl IntoIterator<Item = ResolvedSlot>,
    zero_width_enabled: bool,
) -> Vec<SlotRenderUnit> {
    let resolved_slots = resolved_slots.into_iter();
    let (lower_bound, upper_bound) = resolved_slots.size_hint();
    let mut units = Vec::with_capacity(upper_bound.unwrap_or(lower_bound));

    for resolved in resolved_slots {
        match resolved {
            ResolvedSlot::Text(text) => units.push(SlotRenderUnit::Text(text)),
            ResolvedSlot::Emote(emote) => {
                if zero_width_enabled
                    && emote.zero_width
                    && let Some(SlotRenderUnit::Emote(group)) = units.last_mut()
                {
                    group.add_overlay(emote);
                    continue;
                }

                units.push(SlotRenderUnit::Emote(EmoteRenderUnit::new(emote)));
            }
            ResolvedSlot::TwitchGif { id, url, alt_text } => {
                units.push(SlotRenderUnit::TwitchGif { id, url, alt_text });
            }
            ResolvedSlot::Url { url, embed_id } => {
                units.push(SlotRenderUnit::Url { url, embed_id });
            }
            ResolvedSlot::Mention(username) => units.push(SlotRenderUnit::Mention(username)),
            ResolvedSlot::Emoji { emoji, url } => {
                units.push(SlotRenderUnit::Emoji {
                    emoji,
                    url: url.into(),
                });
            }
        }
    }

    units
}
