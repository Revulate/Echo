use crate::{Emote, EmoteProvider, EmoteScope, emote_scope};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct EmoteView {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub original_name: Option<Arc<str>>,
    pub url: Arc<str>,
    pub provider: EmoteProvider,
    pub channel: Option<Arc<str>>,
    pub url_1x: Option<Arc<str>>,
    pub url_2x: Option<Arc<str>>,
    pub url_4x: Option<Arc<str>>,
    pub is_animated: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl From<Emote> for EmoteView {
    fn from(emote: Emote) -> Self {
        Self {
            id: emote.id,
            name: emote.name,
            original_name: emote.original_name,
            url: emote.url,
            provider: emote.provider,
            channel: emote.channel,
            url_1x: emote.url_1x,
            url_2x: emote.url_2x,
            url_4x: emote.url_4x,
            is_animated: emote.is_animated,
            width: emote.width,
            height: emote.height,
        }
    }
}

impl From<Arc<Emote>> for EmoteView {
    fn from(emote: Arc<Emote>) -> Self {
        Self {
            id: Arc::clone(&emote.id),
            name: Arc::clone(&emote.name),
            original_name: emote.original_name.clone(),
            url: Arc::clone(&emote.url),
            provider: emote.provider,
            channel: emote.channel.clone(),
            url_1x: emote.url_1x.clone(),
            url_2x: emote.url_2x.clone(),
            url_4x: emote.url_4x.clone(),
            is_animated: emote.is_animated,
            width: emote.width,
            height: emote.height,
        }
    }
}

impl EmoteView {
    /// Where this emote can be used.
    pub fn scope(&self) -> EmoteScope<'_> {
        emote_scope(self.channel.as_deref())
    }

    pub fn matches_backend(&self, other: &Emote) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.original_name == other.original_name
            && self.url == other.url
            && self.provider == other.provider
            && self.channel == other.channel
            && self.url_1x == other.url_1x
            && self.url_2x == other.url_2x
            && self.url_4x == other.url_4x
            && self.is_animated == other.is_animated
            && self.width == other.width
            && self.height == other.height
    }

    pub fn source_scale(&self, url: &str) -> &'static str {
        crate::provider_variant_scale(
            self.provider,
            self.url_1x.as_deref(),
            self.url_2x.as_deref(),
            self.url_4x.as_deref(),
            url,
        )
    }

    pub fn url_shared(&self, selected: &str) -> Arc<str> {
        crate::model::shared_variant_url(
            &self.url,
            [&self.url_1x, &self.url_2x, &self.url_4x],
            selected,
        )
    }
}

#[derive(Clone, Debug)]
pub struct EmoteCategoryView {
    pub emotes: Vec<Arc<EmoteView>>,
}

impl From<crate::EmoteCategory> for EmoteCategoryView {
    fn from(category: crate::EmoteCategory) -> Self {
        Self {
            emotes: category
                .emotes
                .into_iter()
                .map(|emote| Arc::new(emote.into()))
                .collect(),
        }
    }
}
