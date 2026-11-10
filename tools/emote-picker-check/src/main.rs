//! Contract: the emote-picker model in `src/components/overlays/emote_picker/`: tab
//! sets, page capacity and load-more rows, search ranking and emoji alias
//! boundaries, favorite/recent/collapsed sections and stability telemetry.

use check_support::{FIXTURE_HEADER, assert_fixture};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

extern crate self as gpui;

#[macro_export]
macro_rules! log_perf {
    ($($arg:tt)*) => {{}};
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hsla;

// The picker only stores display strings in this type; the check never reads them.
#[derive(Clone, Debug, Default)]
pub struct SharedString;

impl From<String> for SharedString {
    fn from(_: String) -> Self {
        Self
    }
}

mod backend {
    use super::*;

    #[derive(Clone, Debug)]
    pub struct Emote {
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

    #[derive(Clone, Debug)]
    pub struct EmoteCategory {
        pub emotes: Vec<Arc<Emote>>,
    }

    // Variant spelling mirrors the production provider API path-included by
    // this harness; changing it here would stop checking source compatibility.
    #[allow(clippy::upper_case_acronyms)]
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub enum EmoteProvider {
        Twitch,
        SevenTV,
        BTTV,
        FFZ,
        Emoji,
    }

    pub fn get_categorized_emotes(_channel: &str) -> Arc<Vec<EmoteCategory>> {
        Arc::new(vec![EmoteCategory {
            emotes: fixture_emotes().into_iter().map(Arc::new).collect(),
        }])
    }

    pub mod emotes {
        pub fn get_emote_cache_version(_channel: &str) -> u32 {
            1
        }

        pub fn get_global_emote_version() -> u32 {
            1
        }
    }

    pub mod emoji {
        pub fn get_emoji_url(emoji: &str, _style: &crate::backend::settings::EmojiStyle) -> String {
            format!("emoji://{emoji}")
        }
    }

    pub mod metrics {
        pub fn record_emote_stage(_stage: &str, _elapsed: std::time::Duration, _items: usize) {}
    }

    pub mod settings {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum EmojiStyle {
            Twemoji,
        }

        pub fn get_emoji_style() -> EmojiStyle {
            EmojiStyle::Twemoji
        }

        pub fn get_emote_image_scale() -> crate::backend::emote_quality::EmoteImageScale {
            crate::backend::emote_quality::EmoteImageScale::TwoX
        }

        pub fn get_enable_seventv_emotes() -> bool {
            true
        }

        pub fn get_enable_bttv_emotes() -> bool {
            true
        }

        pub fn get_enable_ffz_emotes() -> bool {
            true
        }

        pub fn get_layout_settings_version() -> u64 {
            1
        }
    }

    pub mod emote_quality {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
        pub enum EmoteImageScale {
            Auto,
            OneX,
            #[default]
            TwoX,
            FourX,
        }

        impl EmoteImageScale {
            pub fn as_u8(self) -> u8 {
                match self {
                    Self::Auto => 0,
                    Self::OneX => 1,
                    Self::TwoX => 2,
                    Self::FourX => 4,
                }
            }
        }

        pub const NOMINAL_ONE_X_EMOTE_PX: f32 = 28.0;

        const AUTO_SOURCE_HEADROOM: f32 = 1.2;

        // Mirrors src\backend\emote_quality.rs so the check exercises the
        // real Auto tier-selection behavior.
        pub fn resolve_auto_tier(device_scale: f32, rendered_scale: f32) -> EmoteImageScale {
            let effective = device_scale.max(0.1) * rendered_scale.max(0.1) * AUTO_SOURCE_HEADROOM;
            if effective <= 1.001 {
                EmoteImageScale::OneX
            } else if effective <= 2.001 {
                EmoteImageScale::TwoX
            } else {
                EmoteImageScale::FourX
            }
        }

        pub fn select_emote_display_url<'a>(
            default_url: &'a str,
            url_1x: Option<&'a str>,
            url_2x: Option<&'a str>,
            url_4x: Option<&'a str>,
            scale: EmoteImageScale,
        ) -> &'a str {
            match scale {
                EmoteImageScale::OneX => url_1x.or(url_2x).or(url_4x).unwrap_or(default_url),
                EmoteImageScale::TwoX | EmoteImageScale::Auto => {
                    url_2x.or(url_4x).or(url_1x).unwrap_or(default_url)
                }
                EmoteImageScale::FourX => url_4x.or(url_2x).or(url_1x).unwrap_or(default_url),
            }
        }
    }

    fn fixture_emotes() -> Vec<Emote> {
        vec![
            emote(
                "7tv-channel",
                "OMEGALUL",
                "https://cdn/omega",
                EmoteProvider::SevenTV,
                Some("forsen"),
            ),
            emote(
                "7tv-personal",
                "PersonalWow",
                "https://cdn/personal",
                EmoteProvider::SevenTV,
                Some("personal:7tv"),
            ),
            emote(
                "bttv-channel",
                "FeelsOkayMan",
                "https://cdn/bttv",
                EmoteProvider::BTTV,
                Some("forsen"),
            ),
            emote(
                "ffz-channel",
                "CatJAM",
                "https://cdn/ffz",
                EmoteProvider::FFZ,
                Some("forsen"),
            ),
            emote(
                "twitch-sub",
                "SubWave",
                "https://cdn/sub",
                EmoteProvider::Twitch,
                Some("sub:forsen"),
            ),
            emote(
                "twitch-prime",
                "PrimeSmile",
                "https://cdn/prime",
                EmoteProvider::Twitch,
                Some("twitch:prime"),
            ),
            emote(
                "twitch-global",
                "Kappa",
                "https://cdn/kappa",
                EmoteProvider::Twitch,
                None,
            ),
            emote(
                "7tv-global",
                "GlobalSeven",
                "https://cdn/7tv-global",
                EmoteProvider::SevenTV,
                None,
            ),
            emote(
                "bttv-global",
                "GlobalBTTV",
                "https://cdn/bttv-global",
                EmoteProvider::BTTV,
                None,
            ),
            emote(
                "ffz-global",
                "GlobalFFZ",
                "https://cdn/ffz-global",
                EmoteProvider::FFZ,
                None,
            ),
            emote(
                "twitch-event",
                "PrideWave",
                "https://cdn/pride",
                EmoteProvider::Twitch,
                Some("sub:twitchprime"),
            ),
        ]
    }

    fn emote(
        id: &str,
        name: &str,
        url: &str,
        provider: EmoteProvider,
        channel: Option<&str>,
    ) -> Emote {
        Emote {
            id: id.into(),
            name: name.into(),
            original_name: Some(format!("{name} searchable alias").into()),
            url: url.into(),
            provider,
            channel: channel.map(Arc::from),
            url_1x: Some(format!("{url}/1x").into()),
            url_2x: Some(format!("{url}/2x").into()),
            url_4x: Some(format!("{url}/4x").into()),
            is_animated: false,
            width: Some(64),
            height: Some(64),
        }
    }
}

mod state {
    pub struct VersionRegistry;

    impl VersionRegistry {
        pub fn user_emote_version() -> u64 {
            1
        }
    }

    pub mod emote_preferences {
        use super::super::*;

        static PREFS: OnceLock<Mutex<EmotePreferences>> = OnceLock::new();

        #[derive(Clone, Debug)]
        pub struct RecentEmote {
            pub id: String,
            pub provider: String,
            pub name: String,
        }

        impl RecentEmote {
            pub fn key(&self) -> String {
                format!("{}:{}", self.id, self.provider)
            }
        }

        #[derive(Clone, Debug, Default)]
        pub struct EmotePreferencesData {
            pub recently_used: VecDeque<RecentEmote>,
            pub usage_counts: HashMap<String, u32>,
        }

        #[derive(Clone, Default)]
        pub struct EmotePreferences {
            pub data: EmotePreferencesData,
            favorites: HashSet<String>,
            collapsed: HashSet<String>,
        }

        impl EmotePreferences {
            pub fn favorites_set(&self) -> &HashSet<String> {
                &self.favorites
            }

            pub fn get_usage_count(&self, id: &str, _provider: &str) -> u32 {
                match id {
                    "7tv-channel" => 50,
                    "bttv-channel" => 20,
                    _ => 0,
                }
            }

            pub fn is_favorite(&self, id: &str, provider: &str) -> bool {
                self.favorites.contains(&format!("{id}:{provider}"))
            }

            pub fn get_recent(&self, _limit: usize) -> Vec<RecentEmote> {
                vec![RecentEmote {
                    id: "7tv-channel".to_string(),
                    provider: "SevenTV".to_string(),
                    name: "OMEGALUL".to_string(),
                }]
            }

            pub fn is_category_collapsed(&self, id: &str) -> bool {
                self.collapsed.contains(id)
            }

            pub fn toggle_category_collapsed(&mut self, id: &str) {
                if !self.collapsed.insert(id.to_string()) {
                    self.collapsed.remove(id);
                }
            }

            pub fn record_usage(&mut self, _id: &str, _provider: &str, _name: &str) {}

            pub fn toggle_favorite(&mut self, id: &str, provider: &str, _name: &str) {
                let key = format!("{id}:{provider}");
                if !self.favorites.insert(key.clone()) {
                    self.favorites.remove(&key);
                }
            }

            pub fn try_save(&self) {}
        }

        pub fn get_preferences() -> EmotePreferences {
            PREFS
                .get_or_init(|| Mutex::new(EmotePreferences::default()))
                .lock()
                .expect("emote preference lock")
                .clone()
        }

        pub fn get_preferences_mut() -> Option<MutexGuard<'static, EmotePreferences>> {
            Some(
                PREFS
                    .get_or_init(|| Mutex::new(EmotePreferences::default()))
                    .lock()
                    .expect("emote preference lock"),
            )
        }
    }
}

mod components {
    pub mod chat {
        pub mod canvas_renderer {
            pub mod layout_scale {
                pub fn current() -> f32 {
                    1.0
                }
            }
        }
    }
}

mod theme {
    pub mod colors {
        pub fn text_muted() -> crate::Hsla {
            crate::Hsla
        }
    }
}

mod utils {
    pub use echo_core::fuzzy::fuzzy_score;

    pub fn emote_provider_color(_provider: &str) -> crate::Hsla {
        crate::Hsla
    }
}

#[path = "../../../src/components/overlays/emote_picker/core.rs"]
#[allow(dead_code)] // the check drives the picker model, not its view helpers
mod emote_picker_core;

#[path = "../../../src/components/overlays/emote_picker/stability.rs"]
#[allow(dead_code)] // only part of the stability telemetry is asserted here
mod emote_picker_stability;

use backend::{Emote, EmoteProvider};
use emote_picker_core::{
    CategoryHeader, DEFAULT_PICKER_PAGE_EMOTE_LIMIT, EmotePickerCore, EmoteSearchIndex, EmoteTab,
    GridItem, MAX_PICKER_PAGE_EMOTE_LIMIT, calculate_emote_dimensions,
    emote_selection_identity_key, pack_virtual_rows, picker_display_url, picker_page_emote_limit,
    picker_tooltip_text,
};
use emote_picker_stability::{
    PickerCellVisualState, begin_continuity_phase, begin_stationary_window,
    finish_stationary_window, record_animated_frame, record_animated_frame_source, record_cell,
    record_visible_range,
};

#[derive(Debug)]
enum CapacityRow {
    Section,
    Emotes,
    LoadMore { shown: usize, total: usize },
}

fn main() {
    println!("{FIXTURE_HEADER}");

    assert_fixture(
        "picker_inline_tabs",
        tab_labels(EmoteTab::inline_tabs()),
        "Favorites|Recent|Subs|Channel|Global|Emoji",
    );
    assert_fixture(
        "picker_popout_tabs",
        tab_labels(EmoteTab::popout_tabs()),
        "Favorites|Recent|Subs|Channel|Global|Emoji",
    );
    check_picker_capacity();
    check_picker_stability_telemetry();

    let mut picker = EmotePickerCore::new("forsen".to_string());
    picker.load_emotes();
    assert_fixture(
        "picker_channel_sections",
        header_names(&picker),
        "7TV|BetterTTV|FrankerFaceZ|7TV Personal",
    );
    assert_fixture(
        "picker_channel_order",
        emote_names(&picker),
        "OMEGALUL|FeelsOkayMan|CatJAM|PersonalWow",
    );

    picker.select_tab(EmoteTab::Subs);
    assert_fixture("picker_sub_sections", header_names(&picker), "forsen");
    assert_fixture(
        "picker_sub_excludes_global_like",
        emote_names(&picker),
        "SubWave",
    );

    picker.select_tab(EmoteTab::Global);
    assert_fixture(
        "picker_global_sections",
        header_names(&picker),
        "Twitch|7TV|BetterTTV|FrankerFaceZ|Twitch Prime",
    );
    assert_fixture(
        "picker_global_includes_event_sub",
        emote_names(&picker),
        "Kappa|PrideWave|GlobalSeven|GlobalBTTV|GlobalFFZ|PrimeSmile",
    );

    picker.set_search("omegalul searchable".to_string());
    assert_fixture(
        "picker_search_original_name",
        emote_names(&picker),
        "OMEGALUL",
    );
    picker.set_search("OMEGA".to_string());
    assert_fixture(
        "picker_search_dedup",
        picker
            .flat_emotes
            .iter()
            .filter(|emote| emote.id.as_ref() == "7tv-channel")
            .count()
            .to_string(),
        "1",
    );
    check_emoji_search_alias_boundaries();

    let wide = Emote {
        id: "wide".into(),
        name: "Wide".into(),
        original_name: None,
        url: "https://cdn/wide/default".into(),
        provider: EmoteProvider::SevenTV,
        channel: Some("forsen".into()),
        url_1x: Some("https://cdn/wide/1x".into()),
        url_2x: Some("https://cdn/wide/2x".into()),
        url_4x: Some("https://cdn/wide/4x".into()),
        is_animated: false,
        width: Some(1000),
        height: Some(100),
    };
    let (cell_w, cell_h, image_w, image_h) = calculate_emote_dimensions(&wide);
    assert_fixture(
        "picker_display_url_matches_native_density",
        picker_display_url(&wide),
        "https://cdn/wide/1x",
    );
    assert_fixture(
        "picker_wide_preview_preserves_ratio",
        format!("{cell_w:.0}x{cell_h:.0}:{image_w:.0}x{image_h:.0}"),
        "194x56:184x18",
    );
    assert!((image_w / image_h - 10.0).abs() < 0.001);
    let small_native = Emote {
        width: Some(14),
        height: Some(14),
        ..wide.clone()
    };
    assert_fixture(
        "picker_respects_explicit_2x_ceiling",
        picker_display_url(&small_native),
        "https://cdn/wide/2x",
    );
    assert_fixture(
        "picker_tooltip_scope",
        picker_tooltip_text(&wide),
        "Wide - 7TV - Channel forsen",
    );
    let aliased = Emote {
        name: "ChannelWide".into(),
        original_name: Some("Wide".into()),
        ..wide.clone()
    };
    assert_fixture(
        "picker_tooltip_uses_channel_alias",
        picker_tooltip_text(&aliased),
        "ChannelWide - 7TV - Channel forsen",
    );
    let other_channel_alias = Emote {
        name: "OtherWide".into(),
        channel: Some("other_channel".into()),
        ..aliased.clone()
    };
    assert_fixture(
        "picker_tooltip_keeps_same_emote_channel_names_distinct",
        picker_tooltip_text(&other_channel_alias),
        "OtherWide - 7TV - Channel other_channel",
    );

    let square = Arc::new(Emote {
        width: Some(28),
        height: Some(28),
        ..wide.clone()
    });
    let packed_emotes = vec![
        Arc::clone(&square),
        Arc::new(wide.clone()),
        Arc::clone(&square),
        Arc::clone(&square),
        Arc::clone(&square),
    ];
    let packed_items = vec![
        GridItem::Header(CategoryHeader {
            id: "navigation".to_string(),
            name: "Navigation".to_string(),
            icon: String::new(),
            count_str: "5".to_string().into(),
            is_collapsed: false,
            is_special: false,
            provider: Some(EmoteProvider::SevenTV),
        }),
        GridItem::Emotes { range: 0..5 },
    ];
    let (_, shown, _, navigation) = pack_virtual_rows(
        &packed_items,
        &packed_emotes,
        5,
        180.0,
        |_| false,
        |_| (),
        |_, _| (),
        |_, _| (),
    );
    assert_fixture("picker_packed_navigation_shown", shown.to_string(), "5");
    assert_fixture(
        "picker_packed_navigation_visual_down",
        navigation
            .vertical_neighbor(1, 1)
            .unwrap_or_default()
            .to_string(),
        "3",
    );
    assert_fixture(
        "picker_packed_navigation_virtual_row",
        navigation
            .virtual_row_for_index(3)
            .unwrap_or_default()
            .to_string(),
        "3",
    );

    let mut refreshed_fallback = wide.clone();
    refreshed_fallback.id = Arc::from("");
    let fallback_identity = emote_selection_identity_key(&refreshed_fallback);
    refreshed_fallback.url = Arc::from("https://cdn/wide/refreshed");
    assert_fixture(
        "picker_selection_identity_survives_url_refresh",
        (fallback_identity == emote_selection_identity_key(&refreshed_fallback)).to_string(),
        "true",
    );

    let mut prefs_picker = EmotePickerCore::new("forsen".to_string());
    prefs_picker.load_emotes();
    let omega = prefs_picker
        .flat_emotes
        .iter()
        .find(|emote| emote.name.as_ref() == "OMEGALUL")
        .expect("OMEGALUL fixture")
        .clone();
    prefs_picker.toggle_favorite(&omega);
    prefs_picker.select_tab(EmoteTab::Favorites);
    assert_fixture(
        "picker_favorites_section",
        header_names(&prefs_picker),
        "Favorites",
    );
    assert_fixture(
        "picker_favorites_toggle",
        emote_names(&prefs_picker),
        "OMEGALUL",
    );
    assert_fixture(
        "picker_favorite_cache",
        prefs_picker.is_favorite(&omega).to_string(),
        "true",
    );

    prefs_picker.select_tab(EmoteTab::Recent);
    assert_fixture(
        "picker_recent_section",
        header_names(&prefs_picker),
        "Recently Used",
    );
    assert_fixture(
        "picker_recent_provider_match",
        emote_names(&prefs_picker),
        "OMEGALUL",
    );

    prefs_picker.select_tab(EmoteTab::Channel);
    prefs_picker.toggle_category("SevenTV-channel");
    assert_fixture(
        "picker_collapsed_channel_section",
        emote_names(&prefs_picker),
        "FeelsOkayMan|CatJAM|PersonalWow",
    );
}

fn check_emoji_search_alias_boundaries() {
    // Joined into one string, the display name and shortcode of 🤟 fuzzy-match
    // "revu" only across the alias boundary; the index must score each alias
    // on its own.
    assert!(
        utils::fuzzy_score("revu", "love-you gesture love_you_gesture").is_some(),
        "the joined aliases must fuzzy-match across the boundary for this fixture to mean anything"
    );
    let emoji = Arc::new(Emote {
        id: "emoji:love_you_gesture:🤟".into(),
        name: "🤟".into(),
        original_name: Some("love-you gesture love_you_gesture".into()),
        url: "emoji://🤟".into(),
        provider: EmoteProvider::Emoji,
        channel: Some("emoji:people-body".into()),
        url_1x: None,
        url_2x: None,
        url_4x: None,
        is_animated: false,
        width: Some(64),
        height: Some(64),
    });
    let index = EmoteSearchIndex::new("", [emoji]);

    assert_fixture(
        "picker_emoji_alias_boundary_rejects_cross_alias_fuzzy_match",
        index.rank("revu").is_empty().to_string(),
        "true",
    );
    assert_fixture(
        "picker_emoji_shortcode_search",
        index
            .rank("love_you")
            .first()
            .map(|ranked| ranked.emote.name.as_ref())
            .unwrap_or_default(),
        "🤟",
    );
    assert_fixture(
        "picker_emoji_shortcode_is_inline_tab_prefix_match",
        index
            .rank("love_you")
            .first()
            .is_some_and(|ranked| ranked.is_prefix_match)
            .to_string(),
        "true",
    );
}

fn check_picker_stability_telemetry() {
    const MINIMUM_RANGE_OBSERVATIONS: u64 = 4;

    begin_stationary_window(3, 2, 10, 12);
    for pass in 0..MINIMUM_RANGE_OBSERVATIONS {
        // UniformList can issue header-only/empty layout probes before and
        // after the real visible range; these must not look like movement.
        record_visible_range(0, 1, 0);
        record_visible_range(10, 12, 3);
        record_visible_range(12, 12, 0);
        for cell in 0..3 {
            record_cell(cell, PickerCellVisualState::Rendered);
        }
        record_animated_frame(0, (pass % 2) as u16);
        record_animated_frame(1, 2 + (pass % 2) as u16);
    }
    let stable = finish_stationary_window().expect("stable picker telemetry window");
    assert_fixture(
        "picker_stationary_render_stability",
        format!(
            "stable={}:ranges={}:cells={}/{}:animations={}/{}:transitions={}:regressions={}:post_rendered_placeholders={}",
            stable.is_stable(MINIMUM_RANGE_OBSERVATIONS),
            stable.range_observations,
            stable.rendered_unique_cells,
            stable.expected_visible_cells,
            stable.advanced_animated_cells,
            stable.expected_visible_animated_cells,
            stable.animated_frame_transitions,
            stable.rendered_to_placeholder_transitions,
            stable.post_rendered_placeholder_observations,
        ),
        "stable=true:ranges=4:cells=3/3:animations=2/2:transitions=6:regressions=0:post_rendered_placeholders=0",
    );

    begin_stationary_window(3, 2, 10, 12);
    record_visible_range(10, 12, 3);
    for cell in 0..3 {
        record_cell(cell, PickerCellVisualState::Rendered);
    }
    record_animated_frame(0, 0);
    record_animated_frame(1, 2);
    record_visible_range(10, 12, 3);
    record_cell(0, PickerCellVisualState::Rendered);
    record_cell(1, PickerCellVisualState::Placeholder);
    record_cell(2, PickerCellVisualState::Rendered);
    record_animated_frame(0, 1);
    record_visible_range(10, 12, 3);
    for cell in 0..3 {
        record_cell(cell, PickerCellVisualState::Rendered);
    }
    record_animated_frame(0, 0);
    record_animated_frame(1, 3);
    record_visible_range(10, 12, 3);
    for cell in 0..3 {
        record_cell(cell, PickerCellVisualState::Rendered);
    }
    record_animated_frame(0, 1);
    record_animated_frame(1, 2);
    let churn = finish_stationary_window().expect("churning picker telemetry window");
    assert_fixture(
        "picker_stationary_churn_rejected",
        format!(
            "stable={}:regressions={}:post_rendered_placeholders={}",
            churn.is_stable(MINIMUM_RANGE_OBSERVATIONS),
            churn.rendered_to_placeholder_transitions,
            churn.post_rendered_placeholder_observations,
        ),
        "stable=false:regressions=1:post_rendered_placeholders=1",
    );

    begin_stationary_window(3, 2, 10, 12);
    for (pass, range) in [(10, 12), (10, 12), (11, 13), (10, 12)]
        .into_iter()
        .enumerate()
    {
        record_visible_range(range.0, range.1, 3);
        for cell in 0..3 {
            record_cell(cell, PickerCellVisualState::Rendered);
        }
        record_animated_frame(0, (pass % 2) as u16);
        record_animated_frame(1, 2 + (pass % 2) as u16);
    }
    let moved = finish_stationary_window().expect("moving picker telemetry window");
    assert_fixture(
        "picker_stationary_range_change_rejected",
        format!(
            "stable={}:range_changes={}",
            moved.is_stable(MINIMUM_RANGE_OBSERVATIONS),
            moved.visible_range_changes,
        ),
        "stable=false:range_changes=1",
    );

    begin_stationary_window(3, 2, 10, 12);
    for pass in 0..MINIMUM_RANGE_OBSERVATIONS {
        record_visible_range(10, 12, 2);
        record_cell(0, PickerCellVisualState::Rendered);
        record_cell(1, PickerCellVisualState::Rendered);
        record_animated_frame(0, (pass % 2) as u16);
        record_animated_frame(1, 2 + (pass % 2) as u16);
    }
    let incomplete = finish_stationary_window().expect("incomplete picker telemetry window");
    assert_fixture(
        "picker_stationary_incomplete_viewport_rejected",
        format!(
            "stable={}:cells={}/{}",
            incomplete.is_stable(MINIMUM_RANGE_OBSERVATIONS),
            incomplete.rendered_unique_cells,
            incomplete.expected_visible_cells,
        ),
        "stable=false:cells=2/3",
    );

    begin_stationary_window(3, 2, 10, 12);
    for pass in 0..MINIMUM_RANGE_OBSERVATIONS {
        record_visible_range(10, 12, 3);
        for cell in 0..3 {
            record_cell(cell, PickerCellVisualState::Rendered);
        }
        record_animated_frame(0, (pass % 2) as u16);
        record_animated_frame(1, 0);
    }
    let frozen = finish_stationary_window().expect("frozen picker animation telemetry window");
    assert_fixture(
        "picker_stationary_frozen_animation_rejected",
        format!(
            "stable={}:animations={}/{}:transitions={}",
            frozen.is_stable(MINIMUM_RANGE_OBSERVATIONS),
            frozen.advanced_animated_cells,
            frozen.expected_visible_animated_cells,
            frozen.animated_frame_transitions,
        ),
        "stable=false:animations=1/2:transitions=3",
    );

    begin_stationary_window(1, 1, 10, 11);
    for pass in 0..MINIMUM_RANGE_OBSERVATIONS {
        record_visible_range(10, 11, 1);
        record_cell(0, PickerCellVisualState::Rendered);
        record_animated_frame_source(0, "https://cdn/primary", (pass % 2) as u16);
    }
    assert!(begin_continuity_phase());
    for (source, frame) in [
        ("https://cdn/primary", 0),
        ("https://cdn/primary", 1),
        ("https://cdn/compact", 0),
        ("https://cdn/compact", 1),
        ("https://cdn/compact", 0),
    ] {
        record_visible_range(10, 11, 1);
        record_cell(0, PickerCellVisualState::Rendered);
        record_animated_frame_source(0, source, frame);
    }
    let source_switch =
        finish_stationary_window().expect("source-switching picker telemetry window");
    assert_fixture(
        "picker_stationary_source_switch_rejected",
        format!(
            "continuous={}:switches={}:min_transitions={}",
            source_switch.is_continuously_animating(MINIMUM_RANGE_OBSERVATIONS, 2, 10_000),
            source_switch.animated_source_switches,
            source_switch.minimum_animated_cell_transitions,
        ),
        "continuous=false:switches=1:min_transitions=2",
    );
}

fn check_picker_capacity() {
    const LARGE_CATALOG_EMOTE_COUNT: usize = 4_500;
    const OVER_LIMIT_EXTRA_EMOTES: usize = 137;

    let original_limit = std::env::var_os("ECHO_PICKER_PAGE_EMOTE_LIMIT");
    set_picker_page_emote_limit_for_check(None);
    assert_fixture(
        "picker_default_page_capacity",
        picker_page_emote_limit().to_string(),
        "10000",
    );
    set_picker_page_emote_limit_for_check(Some(std::ffi::OsStr::new("999999")));
    assert_fixture(
        "picker_page_capacity_override_clamped",
        picker_page_emote_limit().to_string(),
        "50000",
    );
    set_picker_page_emote_limit_for_check(original_limit.as_deref());

    assert_eq!(DEFAULT_PICKER_PAGE_EMOTE_LIMIT, 10_000);
    assert_eq!(MAX_PICKER_PAGE_EMOTE_LIMIT, 50_000);

    let (large_scale_items, large_scale_emotes) = synthetic_catalog(LARGE_CATALOG_EMOTE_COUNT);
    let (large_rows, large_shown, large_total, large_navigation) = pack_virtual_rows(
        &large_scale_items,
        &large_scale_emotes,
        DEFAULT_PICKER_PAGE_EMOTE_LIMIT,
        560.0,
        |_| false,
        |_| CapacityRow::Section,
        |_, _| CapacityRow::Emotes,
        |shown, total| CapacityRow::LoadMore { shown, total },
    );
    let large_load_more_rows = large_rows
        .iter()
        .filter(|row| matches!(row, CapacityRow::LoadMore { .. }))
        .count();
    assert_fixture(
        "picker_large_catalog_catalog_fully_visible",
        format!("{large_shown}/{large_total}:load_more={large_load_more_rows}"),
        "4500/4500:load_more=0",
    );
    assert_fixture(
        "picker_large_catalog_last_emote_navigable",
        large_navigation
            .virtual_row_for_index(LARGE_CATALOG_EMOTE_COUNT - 1)
            .is_some()
            .to_string(),
        "true",
    );

    let over_limit_count = DEFAULT_PICKER_PAGE_EMOTE_LIMIT + OVER_LIMIT_EXTRA_EMOTES;
    let (over_limit_items, over_limit_emotes) = synthetic_catalog(over_limit_count);
    let (over_limit_rows, over_limit_shown, over_limit_total, _) = pack_virtual_rows(
        &over_limit_items,
        &over_limit_emotes,
        DEFAULT_PICKER_PAGE_EMOTE_LIMIT,
        560.0,
        |_| false,
        |_| CapacityRow::Section,
        |_, _| CapacityRow::Emotes,
        |shown, total| CapacityRow::LoadMore { shown, total },
    );
    let load_more_rows = over_limit_rows
        .iter()
        .filter_map(|row| match row {
            CapacityRow::LoadMore { shown, total } => Some((*shown, *total)),
            CapacityRow::Section | CapacityRow::Emotes => None,
        })
        .collect::<Vec<_>>();
    assert_fixture(
        "picker_exceptional_catalog_retains_load_more",
        format!("{over_limit_shown}/{over_limit_total}:load_more={load_more_rows:?}"),
        "10000/10137:load_more=[(10000, 10137)]",
    );
}

fn set_picker_page_emote_limit_for_check(value: Option<&std::ffi::OsStr>) {
    // SAFETY: this standalone checker is single-threaded, mutates the variable only while
    // checking the synchronous page-capacity path, and restores the original value before
    // continuing. No other thread can read or write the process environment concurrently.
    unsafe {
        match value {
            Some(value) => std::env::set_var("ECHO_PICKER_PAGE_EMOTE_LIMIT", value),
            None => std::env::remove_var("ECHO_PICKER_PAGE_EMOTE_LIMIT"),
        }
    }
}

fn synthetic_catalog(emote_count: usize) -> (Vec<GridItem>, Vec<Arc<Emote>>) {
    let emotes = (0..emote_count)
        .map(|index| {
            Arc::new(Emote {
                id: format!("synthetic-{index:05}").into(),
                name: format!("Synthetic{index:05}").into(),
                original_name: None,
                url: format!("https://cdn.invalid/synthetic/{index:05}").into(),
                provider: EmoteProvider::SevenTV,
                channel: Some("examplechannel".into()),
                url_1x: None,
                url_2x: None,
                url_4x: None,
                is_animated: false,
                width: Some(64),
                height: Some(64),
            })
        })
        .collect();

    (
        vec![
            GridItem::Header(CategoryHeader {
                id: "synthetic-large-channel".to_string(),
                name: "Synthetic Large Channel".to_string(),
                icon: String::new(),
                count_str: emote_count.to_string().into(),
                is_collapsed: false,
                is_special: false,
                provider: Some(EmoteProvider::SevenTV),
            }),
            GridItem::Emotes {
                range: 0..emote_count,
            },
        ],
        emotes,
    )
}

fn tab_labels(tabs: &[EmoteTab]) -> String {
    tabs.iter()
        .map(EmoteTab::label)
        .collect::<Vec<_>>()
        .join("|")
}

fn header_names(picker: &EmotePickerCore) -> String {
    picker
        .grid_items
        .iter()
        .filter_map(|item| match item {
            GridItem::Header(header) => Some(header.name.as_str()),
            GridItem::Emotes { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn emote_names(picker: &EmotePickerCore) -> String {
    picker
        .flat_emotes
        .iter()
        .map(|emote| emote.name.as_ref())
        .collect::<Vec<_>>()
        .join("|")
}
