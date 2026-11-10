//! GPUI-free canonical emote model and policy.

pub mod categories;
pub mod model;
pub mod quality;
pub mod twitch_dimensions;
pub mod view;
pub mod visibility;

pub use categories::{EmoteCategory, group_emotes_into_categories};
pub use model::{
    Emote, EmoteProvider, emote_source_scale, emote_url_shared, normalize_channel_key,
    normalized_channel_key, provider_variant_scale, twitch_emote_url, twitch_emote_url_with_scale,
};
pub use twitch_dimensions::{twitch_base_dimensions, twitch_high_tier_density};
pub use view::{EmoteCategoryView, EmoteView};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn emote(id: &str, provider: EmoteProvider, channel: Option<&str>) -> Arc<Emote> {
        Arc::new(Emote {
            id: Arc::from(id),
            name: Arc::from(id),
            url: Arc::from(format!("https://cdn/{id}/default.webp")),
            provider,
            channel: channel.map(Arc::from),
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: Some(Arc::from(format!("https://cdn/{id}/1x.webp"))),
            url_2x: Some(Arc::from(format!("https://cdn/{id}/2x.webp"))),
            url_4x: Some(Arc::from(format!("https://cdn/{id}/4x.webp"))),
            zero_width: false,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    #[test]
    fn provider_keys_and_source_scales_are_stable() {
        assert_eq!(EmoteProvider::SevenTV.stable_cache_key(), "7tv");
        let twitch = emote("twitch", EmoteProvider::Twitch, None);
        assert_eq!(
            emote_source_scale(&twitch, twitch.url_1x.as_deref().unwrap()),
            "1.0"
        );
        assert_eq!(
            emote_source_scale(&twitch, twitch.url_4x.as_deref().unwrap()),
            "3.0"
        );
        let bttv = emote("bttv", EmoteProvider::BTTV, None);
        assert_eq!(
            emote_source_scale(&bttv, bttv.url_4x.as_deref().unwrap()),
            "3x"
        );
    }

    #[test]
    fn model_view_conversion_and_matches_are_exact() {
        let model = emote("model", EmoteProvider::FFZ, Some("forsen"));
        let view = EmoteView::from(Arc::clone(&model));
        assert!(view.matches_backend(&model));
        assert_eq!(view.source_scale(view.url_2x.as_deref().unwrap()), "2");
    }

    #[test]
    fn shared_urls_reuse_known_arcs_and_copy_unknown_urls() {
        let model = emote("shared", EmoteProvider::SevenTV, None);
        let selected = emote_url_shared(&model, model.url_2x.as_deref().unwrap());
        assert!(Arc::ptr_eq(&selected, model.url_2x.as_ref().unwrap()));
        assert_eq!(
            emote_url_shared(&model, "https://other").as_ref(),
            "https://other"
        );
    }

    #[test]
    fn quality_selection_and_auto_tiers_are_stable() {
        use crate::quality::*;
        assert_eq!(resolve_auto_tier(1.0, 1.0), EmoteImageScale::TwoX);
        assert_eq!(resolve_auto_tier(2.0, 1.0), EmoteImageScale::FourX);
        assert_eq!(
            select_emote_display_url("d", Some("1"), Some("2"), Some("4"), EmoteImageScale::FourX),
            "4"
        );
    }

    #[test]
    fn provider_visibility_keeps_first_party_visible() {
        use crate::visibility::*;
        let hidden = EmoteProviderVisibility {
            seventv: false,
            bttv: false,
            ffz: false,
        };
        assert!(emote_provider_visible(
            EmoteProviderVisibilityKey::Twitch,
            hidden
        ));
        assert!(emote_provider_visible(
            EmoteProviderVisibilityKey::Emoji,
            hidden
        ));
        assert!(!emote_provider_visible(
            EmoteProviderVisibilityKey::SevenTv,
            hidden
        ));
    }

    #[test]
    fn categories_preserve_order_case_matching_and_exclusions() {
        let input = vec![
            emote("global-twitch", EmoteProvider::Twitch, None),
            emote("emoji", EmoteProvider::Emoji, None),
            emote("channel-bttv", EmoteProvider::BTTV, Some("FoRsEn")),
            emote("channel-7tv", EmoteProvider::SevenTV, Some("forsen")),
            emote("sub-b", EmoteProvider::Twitch, Some("sub:BChannel")),
            emote("sub-a", EmoteProvider::Twitch, Some("sub:achannel")),
            emote("personal", EmoteProvider::SevenTV, Some("personal:7tv")),
            emote("global-7tv", EmoteProvider::SevenTV, None),
        ];
        let categories = group_emotes_into_categories(input, "forsen");
        let first_ids = categories
            .iter()
            .map(|category| category.emotes[0].id.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(
            first_ids,
            vec![
                "personal",
                "sub-a",
                "sub-b",
                "channel-7tv",
                "channel-bttv",
                "global-7tv",
                "global-twitch"
            ]
        );
        assert!(
            categories
                .iter()
                .flat_map(|category| category.emotes.iter())
                .all(|emote| emote.id.as_ref() != "emoji")
        );
    }
}
