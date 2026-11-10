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

#[path = "../../../../src/backend/slots/render_units.rs"]
pub mod render_units;
