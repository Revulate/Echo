//! Rich-preview preferences for Links & media.

use crate::backend::settings::{self, EmbedDisplayMode, EmbedThumbnailMode};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, labelled_toggle, setting_reset, setting_row, setting_target,
    settings_defaults, settings_disclosure_with_layout, settings_group, settings_group_contents,
    settings_select,
};
use gpui::*;

const LINK_PREVIEW_MODE: SettingDescriptor = SettingDescriptor {
    id: "links-media.link-preview-mode",
    label: "Link preview mode",
    aliases: &["embed", "preview", "inline", "off"],
    control_id: "embed-display-mode",
    disclosures: &[],
};

const EMBED_THUMBNAILS: SettingDescriptor = SettingDescriptor {
    id: "links-media.embed-thumbnails",
    label: "Thumbnails",
    aliases: &["thumbnail", "image size", "small", "medium", "large"],
    control_id: "embed-thumbnail",
    disclosures: &[],
};

const EMBED_REMOTE_FETCH: SettingDescriptor = SettingDescriptor {
    id: "links-media.embed-remote-fetch",
    label: "Fetch rich preview metadata directly",
    aliases: &["privacy", "metadata", "remote request", "opengraph"],
    control_id: "embed-remote-fetch",
    disclosures: &["previews-more"],
};

const EMBED_SEVENTV: SettingDescriptor = SettingDescriptor {
    id: "links-media.embed-seventv",
    label: "7TV previews",
    aliases: &["7tv", "emote", "user", "provider"],
    control_id: "embed-seventv",
    disclosures: &["previews-more"],
};

const EMBED_SPOTIFY: SettingDescriptor = SettingDescriptor {
    id: "links-media.embed-spotify",
    label: "Spotify previews",
    aliases: &["spotify", "music", "track", "album", "playlist", "provider"],
    control_id: "embed-spotify",
    disclosures: &["previews-more"],
};

const EMBED_STEAM: SettingDescriptor = SettingDescriptor {
    id: "links-media.embed-steam",
    label: "Steam previews",
    aliases: &["steam", "steamdb", "protondb", "game", "app", "provider"],
    control_id: "embed-steam",
    disclosures: &["previews-more"],
};

/// Links & Media › Link previews.
pub const LINKS_MEDIA_LINK_PREVIEWS_SETTINGS: &[SettingDescriptor] = &[
    LINK_PREVIEW_MODE,
    EMBED_THUMBNAILS,
    EMBED_SEVENTV,
    EMBED_SPOTIFY,
    EMBED_STEAM,
    EMBED_REMOTE_FETCH,
];

pub fn render_embeds_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let display = settings::get_embed_display_mode();
    let thumbnails = settings::get_embed_thumbnail_mode();
    let seventv = settings::get_embed_seventv_enabled();
    let spotify = settings::get_embed_spotify_enabled();
    let steam = settings::get_embed_steam_enabled();
    let remote = settings::get_embed_remote_fetch_enabled();
    let display_select = ui.select(LINK_PREVIEW_MODE.control_id, cx);
    let thumbnail_select = ui.select(EMBED_THUMBNAILS.control_id, cx);
    let details = ui.disclosure("previews-more", cx);
    let stacked = ui.stacked;
    let modified_count = usize::from(seventv != defaults.embed_providers.seventv)
        + usize::from(spotify != defaults.embed_providers.spotify)
        + usize::from(steam != defaults.embed_providers.steam)
        + usize::from(remote != defaults.embed_remote_fetch_enabled);
    let display_modified = display != defaults.embed_display_mode;
    let thumbnail_modified = thumbnails != defaults.embed_thumbnail_mode;
    settings_group(
        "links-previews", "Previews", false,
        settings_group_contents(
            "links-previews-content", "Previews", true,
            div().w_full().min_w_0().flex().flex_col()
                .child(setting_target(LINK_PREVIEW_MODE, setting_row(
                    LINK_PREVIEW_MODE.id, LINK_PREVIEW_MODE.label,
                    Some("None skips fetching. Manual opens previews from the link context menu; Always shows them automatically."),
                    settings_select(
                        LINK_PREVIEW_MODE.control_id, LINK_PREVIEW_MODE.label,
                        &[
                            SelectOption::new("none", "None"),
                            SelectOption::new("click", "Manual"),
                            SelectOption::new("always", "Always"),
                        ],
                        preview_mode_value(&display), &display_select, cx,
                        |value, cx| {
                            settings::set_embed_display_mode(match value.as_str() {
                                "always" => EmbedDisplayMode::Always,
                                "click" => EmbedDisplayMode::Click,
                                _ => EmbedDisplayMode::None,
                            });
                            cx.notify();
                        },
                    ),
                    display_modified,
                    display_modified.then(|| setting_reset("reset-link-preview-mode", LINK_PREVIEW_MODE.label, cx, move |cx| {
                        settings::set_embed_display_mode(defaults.embed_display_mode.clone());
                        cx.notify();
                    })),
                    stacked,
                )))
                .child(setting_target(EMBED_THUMBNAILS, setting_row(
                    EMBED_THUMBNAILS.id, EMBED_THUMBNAILS.label,
                    Some("None hides thumbnails. Small is 96×54; Medium 160×90; Large 240×135."),
                    settings_select(
                        EMBED_THUMBNAILS.control_id, EMBED_THUMBNAILS.label,
                        &[
                            SelectOption::new("none", "None"),
                            SelectOption::new("small", "Small"),
                            SelectOption::new("medium", "Medium"),
                            SelectOption::new("large", "Large"),
                        ],
                        thumbnails.settings_value(), &thumbnail_select, cx,
                        |value, cx| {
                            settings::set_embed_thumbnail_mode(EmbedThumbnailMode::from_settings_value(&value));
                            cx.notify();
                        },
                    ),
                    thumbnail_modified,
                    thumbnail_modified.then(|| setting_reset("reset-preview-thumbnails", EMBED_THUMBNAILS.label, cx, move |cx| {
                        settings::set_embed_thumbnail_mode(defaults.embed_thumbnail_mode);
                        cx.notify();
                    })),
                    stacked,
                )))
                .child(settings_disclosure_with_layout(
                    "previews-more", "Preview options", Some("Providers and direct metadata requests"),
                    modified_count, &details, stacked, cx,
                    move |cx| {
                        div().w_full().min_w_0().flex().flex_col()
                            .child(preview_toggle(EMBED_SEVENTV, "Preview supported 7TV emote and user pages.", (seventv,defaults.embed_providers.seventv), stacked,cx,settings::set_embed_seventv_enabled))
                            .child(preview_toggle(EMBED_SPOTIFY, "Preview supported Spotify tracks, albums, playlists, artists, episodes, and shows.", (spotify,defaults.embed_providers.spotify), stacked,cx,settings::set_embed_spotify_enabled))
                            .child(preview_toggle(EMBED_STEAM, "Preview Steam Store, SteamDB, and ProtonDB application pages.", (steam,defaults.embed_providers.steam), stacked,cx,settings::set_embed_steam_enabled))
                            .child(preview_toggle(EMBED_REMOTE_FETCH, "Contacts linked sites directly for titles and thumbnails. Leave off to avoid these metadata requests.", (remote,defaults.embed_remote_fetch_enabled), stacked,cx,settings::set_embed_remote_fetch_enabled))
                    },
                )),
        ),
    )
}

fn preview_mode_value(mode: &EmbedDisplayMode) -> &'static str {
    match mode {
        EmbedDisplayMode::None => "none",
        EmbedDisplayMode::Click => "click",
        EmbedDisplayMode::Always => "always",
    }
}

fn preview_toggle<T: 'static>(
    descriptor: SettingDescriptor,
    help: &'static str,
    (value, default): (bool, bool),
    stacked: bool,
    cx: &mut Context<T>,
    setter: fn(bool),
) -> impl IntoElement + use<T> {
    let modified = value != default;
    setting_target(
        descriptor,
        crate::components::settings::widgets::setting_compact_row(
            descriptor.id,
            descriptor.label,
            Some(help),
            labelled_toggle(
                descriptor.control_id,
                descriptor.label,
                value,
                cx,
                move |value, cx| {
                    setter(value);
                    cx.notify();
                },
            ),
            modified,
            modified.then(|| {
                setting_reset(
                    format!("reset-{}", descriptor.control_id),
                    descriptor.label,
                    cx,
                    move |cx| {
                        setter(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}
