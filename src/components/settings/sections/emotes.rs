//! Chat emote settings.

use crate::backend::{
    self,
    settings::{self, EmojiStyle, EmoteDisplayFlag, EmoteDisplayRule, EmoteRuleScope},
};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, action_button_keyed, empty_state, labelled_toggle, setting_reset,
    setting_row, setting_target, settings_defaults, settings_disclosure_with_layout,
    settings_group_contents, settings_select, settings_subheading, settings_summary_disclosure,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

const EMOTES_DISCLOSURE: &str = "chat-more-emotes";
const EMOTE_RULES_DISCLOSURE: &str = "chat-emote-display-rules";

const EMOJI_STYLE: SettingDescriptor = SettingDescriptor {
    id: "emotes.emoji-style",
    label: "Emoji style",
    aliases: &[
        "twemoji",
        "noto",
        "native emoji",
        "emoji font",
        "Emoji Style",
    ],
    control_id: "emoji-style",
    disclosures: &[EMOTES_DISCLOSURE],
};
const EMOTE_SPACING: SettingDescriptor = SettingDescriptor {
    id: "emotes.spacing",
    label: "No spacing between adjacent emotes",
    aliases: &["emote spacing", "gap", "padding"],
    control_id: "emote-spacing",
    disclosures: &[EMOTES_DISCLOSURE],
};
const ZERO_WIDTH_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.zero-width",
    label: "Enable zero-width emotes",
    aliases: &["overlay emote", "zero width", "stacked"],
    control_id: "zero-width",
    disclosures: &[EMOTES_DISCLOSURE],
};
const ANIMATE_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.animate",
    label: "Animate emotes",
    aliases: &["animated emotes", "gif", "playback"],
    control_id: "animate-emotes",
    disclosures: &[],
};
const SEVENTV_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.seventv",
    label: "7TV emotes",
    aliases: &["7tv", "seventv", "provider"],
    control_id: "7tv-emotes",
    disclosures: &[EMOTES_DISCLOSURE],
};
const BTTV_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.bttv",
    label: "BetterTTV emotes",
    aliases: &["bttv", "betterttv", "provider"],
    control_id: "bttv-emotes",
    disclosures: &[EMOTES_DISCLOSURE],
};
const FFZ_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.ffz",
    label: "FrankerFaceZ emotes",
    aliases: &["ffz", "frankerfacez", "provider"],
    control_id: "ffz-emotes",
    disclosures: &[EMOTES_DISCLOSURE],
};
const ACCOUNT_EMOTES: SettingDescriptor = SettingDescriptor {
    id: "emotes.account",
    label: "Twitch subscriptions and personal 7TV",
    aliases: &["account emotes", "subscription", "refresh", "personal"],
    control_id: "account-emotes-refresh",
    disclosures: &[EMOTES_DISCLOSURE],
};
const FFZ_BADGES: SettingDescriptor = SettingDescriptor {
    id: "emotes.ffz-badges",
    label: "FrankerFaceZ badges",
    aliases: &["ffz", "third party badge"],
    control_id: "ffz-badges",
    disclosures: &[EMOTES_DISCLOSURE],
};
const BTTV_BADGES: SettingDescriptor = SettingDescriptor {
    id: "emotes.bttv-badges",
    label: "BetterTTV badges",
    aliases: &["bttv", "third party badge"],
    control_id: "bttv-badges",
    disclosures: &[EMOTES_DISCLOSURE],
};
const BTTV_PRO_BADGES: SettingDescriptor = SettingDescriptor {
    id: "emotes.bttv-pro-badges",
    label: "BetterTTV Pro badges",
    aliases: &["bttv pro", "third party badge"],
    control_id: "bttv-pro-badges",
    disclosures: &[EMOTES_DISCLOSURE],
};
const FFZ_BOT_BADGES: SettingDescriptor = SettingDescriptor {
    id: "emotes.ffz-bot-badges",
    label: "FrankerFaceZ bot badges",
    aliases: &["ffz bot", "third party badge"],
    control_id: "ffz-bot-badges",
    disclosures: &[EMOTES_DISCLOSURE],
};
const SEVENTV_NAME_PAINTS: SettingDescriptor = SettingDescriptor {
    id: "emotes.seventv-name-paints",
    label: "7TV name paints",
    aliases: &["paint", "gradient", "cosmetic", "username color"],
    control_id: "7tv-name-paints",
    disclosures: &[EMOTES_DISCLOSURE],
};
const SEVENTV_COSMETIC_BADGES: SettingDescriptor = SettingDescriptor {
    id: "emotes.seventv-cosmetic-badges",
    label: "Cosmetic badges",
    aliases: &["7tv", "cosmetic", "badge"],
    control_id: "7tv-cosmetic-badges",
    disclosures: &[EMOTES_DISCLOSURE],
};
const SEVENTV_PROFILE_AVATARS: SettingDescriptor = SettingDescriptor {
    id: "emotes.seventv-profile-avatars",
    label: "Profile avatars",
    aliases: &["7tv", "avatar", "user card"],
    control_id: "7tv-profile-avatars",
    disclosures: &[EMOTES_DISCLOSURE],
};

const EMOTE_DISPLAY_RULES: SettingDescriptor = SettingDescriptor {
    id: "emotes.display-rules",
    label: "Emote display rules",
    aliases: &[
        "show emote as text",
        "emote as text",
        "still image",
        "stop emote animation",
        "freeze animated emote",
        "ignore emote",
        "photosensitive",
    ],
    control_id: "emote-display-rules",
    disclosures: &[EMOTE_RULES_DISCLOSURE],
};

pub const CHAT_EMOTES_PRIMARY_SETTINGS: &[SettingDescriptor] = &[ANIMATE_EMOTES];
pub const CHAT_EMOTE_RULES_SETTINGS: &[SettingDescriptor] = &[EMOTE_DISPLAY_RULES];
pub const CHAT_EMOTES_MORE_SETTINGS: &[SettingDescriptor] = &[
    EMOJI_STYLE,
    EMOTE_SPACING,
    ZERO_WIDTH_EMOTES,
    SEVENTV_EMOTES,
    BTTV_EMOTES,
    FFZ_EMOTES,
    ACCOUNT_EMOTES,
    FFZ_BADGES,
    BTTV_BADGES,
    BTTV_PRO_BADGES,
    FFZ_BOT_BADGES,
    SEVENTV_NAME_PAINTS,
    SEVENTV_COSMETIC_BADGES,
    SEVENTV_PROFILE_AVATARS,
];

// Legacy anchor tables retain their original membership while routing to Chat.
pub const EMOTES_DISPLAY_SETTINGS: &[SettingDescriptor] = &[
    EMOJI_STYLE,
    EMOTE_SPACING,
    ZERO_WIDTH_EMOTES,
    ANIMATE_EMOTES,
];
pub const EMOTES_PROVIDERS_SETTINGS: &[SettingDescriptor] =
    &[SEVENTV_EMOTES, BTTV_EMOTES, FFZ_EMOTES, ACCOUNT_EMOTES];
pub const EMOTES_COSMETICS_SETTINGS: &[SettingDescriptor] = &[
    FFZ_BADGES,
    BTTV_BADGES,
    BTTV_PRO_BADGES,
    FFZ_BOT_BADGES,
    SEVENTV_NAME_PAINTS,
    SEVENTV_COSMETIC_BADGES,
    SEVENTV_PROFILE_AVATARS,
];

#[derive(Clone)]
struct EmoteSettings {
    remove_spacing: bool,
    emoji_style: EmojiStyle,
    zero_width_enabled: bool,
    enable_seventv_emotes: bool,
    enable_bttv_emotes: bool,
    enable_ffz_emotes: bool,
    show_ffz_badges: bool,
    show_bttv_badges: bool,
    show_bttv_pro_badges: bool,
    show_ffz_bot_badges: bool,
    show_seventv_name_paints: bool,
    show_seventv_paint_shadows: bool,
    show_seventv_cosmetic_badges: bool,
    show_seventv_profile_avatars: bool,
}

fn current() -> EmoteSettings {
    EmoteSettings {
        remove_spacing: settings::get_remove_emote_spacing(),
        emoji_style: settings::get_emoji_style(),
        zero_width_enabled: settings::get_zero_width_enabled(),
        enable_seventv_emotes: settings::get_enable_seventv_emotes(),
        enable_bttv_emotes: settings::get_enable_bttv_emotes(),
        enable_ffz_emotes: settings::get_enable_ffz_emotes(),
        show_ffz_badges: settings::get_show_ffz_badges(),
        show_bttv_badges: settings::get_show_bttv_badges(),
        show_bttv_pro_badges: settings::get_show_bttv_pro_badges(),
        show_ffz_bot_badges: settings::get_show_ffz_bot_badges(),
        show_seventv_name_paints: settings::get_show_seventv_name_paints(),
        show_seventv_paint_shadows: settings::get_show_seventv_paint_shadows(),
        show_seventv_cosmetic_badges: settings::get_show_seventv_cosmetic_badges(),
        show_seventv_profile_avatars: settings::get_show_seventv_profile_avatars(),
    }
}

/// The one emote preference shown with Chat's primary rows.
pub fn render_chat_emotes_primary<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let value = settings::get_animate_emotes();
    let default = settings_defaults().animate_emotes;
    let modified = value != default;
    setting_target(
        ANIMATE_EMOTES,
        crate::components::settings::widgets::setting_compact_row(
            ANIMATE_EMOTES.id,
            ANIMATE_EMOTES.label,
            Some("Play animated emotes in chat, tooltips, and previews"),
            labelled_toggle(
                SharedString::from(format!("toggle-{}", ANIMATE_EMOTES.control_id)),
                ANIMATE_EMOTES.label,
                value,
                cx,
                |enabled, cx| {
                    settings::set_animate_emotes(enabled);
                    cx.notify();
                },
            ),
            modified,
            modified.then(|| {
                setting_reset(
                    "reset-animate-emotes",
                    ANIMATE_EMOTES.label,
                    cx,
                    move |cx| {
                        settings::set_animate_emotes(default);
                        cx.notify();
                    },
                )
            }),
            ui.stacked,
        ),
    )
}

/// The contextual emote controls, owned by the enclosing Emotes surface.
pub fn render_chat_emotes_more<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let current = current();
    let defaults = settings_defaults();
    let emotes = ui.disclosure(EMOTES_DISCLOSURE, cx);
    let emoji_select = ui.select(EMOJI_STYLE.control_id, cx);
    let stacked = ui.stacked;
    let modified_count = usize::from(current.emoji_style != defaults.emoji_style)
        + usize::from(current.remove_spacing != defaults.remove_emote_spacing)
        + usize::from(current.zero_width_enabled != defaults.zero_width_enabled)
        + usize::from(current.enable_seventv_emotes != defaults.enable_seventv_emotes)
        + usize::from(current.enable_bttv_emotes != defaults.enable_bttv_emotes)
        + usize::from(current.enable_ffz_emotes != defaults.enable_ffz_emotes)
        + usize::from(current.show_ffz_badges != defaults.show_ffz_badges)
        + usize::from(current.show_bttv_badges != defaults.show_bttv_badges)
        + usize::from(current.show_bttv_pro_badges != defaults.show_bttv_pro_badges)
        + usize::from(current.show_ffz_bot_badges != defaults.show_ffz_bot_badges);
    let paints_modified = current.show_seventv_name_paints != defaults.show_seventv_name_paints
        || current.show_seventv_paint_shadows != defaults.show_seventv_paint_shadows;
    let modified_count = modified_count
        + usize::from(paints_modified)
        + usize::from(
            current.show_seventv_cosmetic_badges != defaults.show_seventv_cosmetic_badges,
        )
        + usize::from(
            current.show_seventv_profile_avatars != defaults.show_seventv_profile_avatars,
        );

    settings_disclosure_with_layout(
        EMOTES_DISCLOSURE,
        "Emotes, providers and badges",
        Some("Emoji, spacing, providers, badges, and cosmetics"),
        modified_count,
        &emotes,
        stacked,
        cx,
        move |cx| {
            let emoji_modified = current.emoji_style != defaults.emoji_style;
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(Spacing::MD))
                .child(settings_subheading("Emotes"))
                .child(setting_target(
                    EMOJI_STYLE,
                    setting_row(
                        EMOJI_STYLE.id,
                        EMOJI_STYLE.label,
                        Some("Choose how standard emoji render"),
                        settings_select(
                            EMOJI_STYLE.control_id,
                            EMOJI_STYLE.label,
                            &[
                                SelectOption::new("Twemoji", "Twemoji"),
                                SelectOption::new("Noto", "Noto"),
                                SelectOption::new("Native", "Native"),
                            ],
                            emoji_style_name(current.emoji_style),
                            &emoji_select,
                            cx,
                            |value, cx| {
                                settings::set_emoji_style(emoji_style_from_name(&value));
                                cx.notify();
                            },
                        ),
                        emoji_modified,
                        emoji_modified.then(|| {
                            setting_reset("reset-emoji-style", EMOJI_STYLE.label, cx, move |cx| {
                                settings::set_emoji_style(defaults.emoji_style);
                                cx.notify();
                            })
                        }),
                        stacked,
                    ),
                ))
                .child(bool_row(
                    EMOTE_SPACING,
                    current.remove_spacing,
                    defaults.remove_emote_spacing,
                    "Remove gaps between consecutive emotes while keeping text spacing",
                    stacked,
                    cx,
                    settings::set_remove_emote_spacing,
                ))
                .child(bool_row(
                    ZERO_WIDTH_EMOTES,
                    current.zero_width_enabled,
                    defaults.zero_width_enabled,
                    "Allow emotes that overlay other emotes",
                    stacked,
                    cx,
                    settings::set_zero_width_enabled,
                ))
                .child(settings_subheading("Providers"))
                .child(bool_row(SEVENTV_EMOTES, current.enable_seventv_emotes, defaults.enable_seventv_emotes, "Show global, channel, and personal 7TV emotes in chat and picker", stacked, cx, settings::set_enable_seventv_emotes))
                .child(bool_row(BTTV_EMOTES, current.enable_bttv_emotes, defaults.enable_bttv_emotes, "Show global, channel, and shared BetterTTV emotes in chat and picker", stacked, cx, settings::set_enable_bttv_emotes))
                .child(bool_row(FFZ_EMOTES, current.enable_ffz_emotes, defaults.enable_ffz_emotes, "Show global and channel FrankerFaceZ emotes in chat and picker", stacked, cx, settings::set_enable_ffz_emotes))
                .child(setting_target(ACCOUNT_EMOTES, action_row(ACCOUNT_EMOTES, "Refetch account-specific emotes for the active login.", stacked, cx)))
                .child(settings_subheading("Badges"))
                .child(bool_row(FFZ_BADGES, current.show_ffz_badges, defaults.show_ffz_badges, "Show FrankerFaceZ user and channel badges when available", stacked, cx, settings::set_show_ffz_badges))
                .child(bool_row(BTTV_BADGES, current.show_bttv_badges, defaults.show_bttv_badges, "Show BetterTTV user and channel badges when available", stacked, cx, settings::set_show_bttv_badges))
                .child(bool_row(BTTV_PRO_BADGES, current.show_bttv_pro_badges, defaults.show_bttv_pro_badges, "Show BetterTTV Pro badges separately from regular BetterTTV badges", stacked, cx, settings::set_show_bttv_pro_badges))
                .child(bool_row(FFZ_BOT_BADGES, current.show_ffz_bot_badges, defaults.show_ffz_bot_badges, "Show FrankerFaceZ bot badges where provider metadata marks them", stacked, cx, settings::set_show_ffz_bot_badges))
                .child(settings_subheading("Cosmetics"))
                .child(setting_target(SEVENTV_NAME_PAINTS, crate::components::settings::widgets::setting_compact_row(
                    SEVENTV_NAME_PAINTS.id,
                    SEVENTV_NAME_PAINTS.label,
                    Some("Show complete username paints, including gradients, images, shadows, and strokes"),
                    labelled_toggle(
                        SharedString::from(format!("toggle-{}", SEVENTV_NAME_PAINTS.control_id)),
                        SEVENTV_NAME_PAINTS.label,
                        current.show_seventv_name_paints && current.show_seventv_paint_shadows,
                        cx,
                        |enabled, cx| {
                            settings::set_show_seventv_paint_shadows(enabled);
                            settings::set_show_seventv_name_paints(enabled);
                            cx.notify();
                        },
                    ),
                    paints_modified,
                    paints_modified.then(|| setting_reset(
                        "reset-7tv-name-paints",
                        SEVENTV_NAME_PAINTS.label,
                        cx,
                        move |cx| {
                            settings::set_show_seventv_name_paints(defaults.show_seventv_name_paints);
                            settings::set_show_seventv_paint_shadows(defaults.show_seventv_paint_shadows);
                            cx.notify();
                        },
                    )),
                    stacked,
                )))
                .child(bool_row(SEVENTV_COSMETIC_BADGES, current.show_seventv_cosmetic_badges, defaults.show_seventv_cosmetic_badges, "Render 7TV cosmetic badges in chat and user cards", stacked, cx, settings::set_show_seventv_cosmetic_badges))
                .child(bool_row(SEVENTV_PROFILE_AVATARS, current.show_seventv_profile_avatars, defaults.show_seventv_profile_avatars, "Use 7TV profile avatar images in user cards", stacked, cx, settings::set_show_seventv_profile_avatars))
        },
    )
}

/// Per-emote "Show as text" / "Use still image" rules, added from an emote's
/// context menu in chat: a rule-count summary row, last in the Emotes group,
/// that opens the rule list.
pub fn render_emote_display_rules<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rules = settings::get_emote_display_rules();
    let summary = match rules.len() {
        0 => "None".to_string(),
        1 => "1 rule".to_string(),
        count => format!("{count} rules"),
    };
    let disclosure = ui.disclosure(EMOTE_RULES_DISCLOSURE, cx);

    settings_summary_disclosure(
        EMOTE_RULES_DISCLOSURE,
        EMOTE_DISPLAY_RULES.label,
        &summary,
        &disclosure,
        cx,
        move |cx| {
            settings_group_contents(
                "chat-emote-display-rules-list",
                EMOTE_DISPLAY_RULES.label,
                false,
                setting_target(
                    EMOTE_DISPLAY_RULES,
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(rules.is_empty(), |list| {
                            list.child(empty_state(
                                "Right-click an emote in chat to show it as text or as a still image.",
                            ))
                        })
                        .children(
                            rules
                                .into_iter()
                                .map(|rule| render_emote_rule_row(rule, cx)),
                        ),
                ),
            )
        },
    )
}

fn render_emote_rule_row<T: 'static>(
    rule: EmoteDisplayRule,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let (scope_label, scope_key) = match &rule.scope {
        EmoteRuleScope::Everywhere => ("Everywhere".to_string(), "*".to_string()),
        EmoteRuleScope::Channel(login) => (format!("#{login}"), format!("#{login}")),
    };
    let provider_label = match rule.emote.provider.as_str() {
        "SevenTV" => "7TV",
        provider => provider,
    }
    .to_string();
    // Raw, unslugged identity: provider and id are case-sensitive, and the
    // scope key keeps Everywhere distinct from a channel named "everywhere".
    let key = format!("{}:{}:{}", rule.emote.provider, rule.emote.id, scope_key);
    let active = rule.show_as_text || rule.still_image;
    let text_toggle = render_emote_rule_flag(
        &rule,
        EmoteDisplayFlag::ShowAsText,
        format!("emote-rule-text:{key}"),
        "Show as text",
        format!("Show {} as text, {}", rule.emote.name, scope_label),
        cx,
    );
    let still_toggle = render_emote_rule_flag(
        &rule,
        EmoteDisplayFlag::StillImage,
        format!("emote-rule-still:{key}"),
        "Use still image",
        format!("Use a still image for {}, {}", rule.emote.name, scope_label),
        cx,
    );
    let chip = |label: String| {
        div()
            .px_1p5()
            .py_0p5()
            .rounded(Radii::sm())
            .bg(colors::bg_surface())
            .border_1()
            .border_color(colors::border())
            .text_xs()
            .text_color(colors::text_secondary())
            .child(label)
    };
    let (emote, scope) = (rule.emote.clone(), rule.scope.clone());

    div()
        .w_full()
        .p_3()
        .bg(colors::bg_elevated())
        .rounded(Radii::md())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .font_family("monospace")
                        .whitespace_normal()
                        .text_color(if active {
                            colors::text_primary()
                        } else {
                            colors::text_muted()
                        })
                        .child(rule.emote.name.clone()),
                )
                .child(chip(provider_label))
                .child(chip(scope_label)),
        )
        .child(text_toggle)
        .child(still_toggle)
        .child(action_button_keyed(
            format!("delete-emote-rule:{key}"),
            "Delete",
            true,
            cx,
            move |cx| {
                settings::remove_emote_display_rule(&emote, &scope);
                cx.notify();
            },
        ))
}

/// One rule flag switch. The switch carries the accessible name; the visible
/// caption beside it is plain text. The switch writes only its own flag, so a
/// row rendered before another change cannot overwrite the other flag.
fn render_emote_rule_flag<T: 'static>(
    rule: &EmoteDisplayRule,
    flag: EmoteDisplayFlag,
    id: String,
    caption: &'static str,
    accessible_label: String,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let is_on = match flag {
        EmoteDisplayFlag::ShowAsText => rule.show_as_text,
        EmoteDisplayFlag::StillImage => rule.still_image,
    };
    let (emote, scope) = (rule.emote.clone(), rule.scope.clone());
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(labelled_toggle(
            SharedString::from(id),
            &accessible_label,
            is_on,
            cx,
            move |enabled, cx| {
                settings::set_emote_display_rule_flag(&emote, &scope, flag, enabled);
                cx.notify();
            },
        ))
        .child(
            div()
                .text_xs()
                .text_color(colors::text_secondary())
                .child(caption),
        )
}

fn bool_row<T: 'static, F>(
    descriptor: SettingDescriptor,
    value: bool,
    default: bool,
    help: &'static str,
    stacked: bool,
    cx: &mut Context<T>,
    set: F,
) -> impl IntoElement + use<T, F>
where
    F: Fn(bool) + Clone + 'static,
{
    let reset = set.clone();
    let modified = value != default;
    setting_target(
        descriptor,
        crate::components::settings::widgets::setting_compact_row(
            descriptor.id,
            descriptor.label,
            Some(help),
            labelled_toggle(
                SharedString::from(format!("toggle-{}", descriptor.control_id)),
                descriptor.label,
                value,
                cx,
                move |enabled, cx| {
                    set(enabled);
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
                        reset(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

fn action_row<T: 'static>(
    descriptor: SettingDescriptor,
    help: &'static str,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    crate::components::settings::widgets::setting_compact_row(
        descriptor.id,
        descriptor.label,
        Some(help),
        action_button_keyed("emotes-account-refresh", "Refresh", false, cx, |cx| {
            backend::refresh_authenticated_user_emotes_async();
            cx.notify();
        }),
        false,
        None,
        stacked,
    )
}

fn emoji_style_name(style: EmojiStyle) -> &'static str {
    match style {
        EmojiStyle::Twemoji => "Twemoji",
        EmojiStyle::Noto => "Noto",
        EmojiStyle::Native => "Native",
    }
}

fn emoji_style_from_name(value: &str) -> EmojiStyle {
    match value {
        "Noto" => EmojiStyle::Noto,
        "Native" => EmojiStyle::Native,
        _ => EmojiStyle::Twemoji,
    }
}
