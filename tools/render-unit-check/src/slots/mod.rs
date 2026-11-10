#[derive(Clone, Debug)]
pub enum MessageSlot {}

pub mod resolver {
    use crate::backend::emotes::Emote;
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    pub enum ResolvedSlot {
        Text(Arc<str>),
        Emote(Arc<Emote>),
        Url {
            url: Arc<str>,
            embed_id: u32,
        },
        Mention(Arc<str>),
        Emoji {
            emoji: Arc<str>,
            url: String,
        },
        TwitchGif {
            id: Arc<str>,
            url: Arc<str>,
            alt_text: Arc<str>,
        },
    }

    pub struct SlotResolver;

    impl SlotResolver {
        pub fn resolve(&self, slot: &super::MessageSlot) -> ResolvedSlot {
            match *slot {}
        }
    }
}

// `resolve_render_units` needs `MessageSlot` and `SlotResolver` to compile; the
// check feeds already-resolved slots to `group_resolved_slots` instead.
#[path = "../../../../src/backend/slots/render_units.rs"]
#[allow(dead_code)]
pub mod render_units;
