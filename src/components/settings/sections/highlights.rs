//! Highlights settings section with interactive controls wired to backend

use crate::backend;
use crate::backend::highlight_decision::{
    HighlightSample, HighlightTestOutcome, HighlightTestSound, test_highlight_sample,
};
use crate::backend::settings::{
    HighlightBlacklistUser, HighlightRule, HighlightRuleTarget, add_highlight_blacklist_user,
    add_highlight_rule, get_highlight_blacklist_users, get_highlight_rules,
    remove_highlight_blacklist_user, remove_highlight_rule, update_highlight_blacklist_user,
    update_highlight_rule,
};
use crate::components::settings::draft_input::{self, DraftField, DraftInput};
use crate::components::settings::editor_state::{EditorEditResult, EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsLocation, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::settings_surface::SettingsOpenRequest;
use crate::components::settings::widgets::{
    ColorAlphaMode, ColorEditorOptions, SelectOption, SettingsMetrics, SettingsPageUi,
    action_button_keyed, button as render_highlight_blacklist_button, button_keyed, color_editor,
    color_setting_row, empty_state, labelled_toggle, reset_color_editor_draft, select_buttons,
    setting_colors_match, setting_reset_with_window, setting_target, settings_defaults,
    settings_disclosure_with_layout, settings_group, settings_group_contents, settings_menu_button,
    settings_subtopic, settings_summary_disclosure,
    toggle_small_static as render_highlight_blacklist_small_toggle,
};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::LazyLock;
use uuid::Uuid;

const HIGHLIGHTS_BEHAVIOR_DISCLOSURE: &str = "highlights-behavior";
const HIGHLIGHTS_COLORS_DISCLOSURE: &str = "highlights-colors";
const HIGHLIGHTS_RULES_DISCLOSURE: &str = "highlights-rules";
const HIGHLIGHT_TESTER_DISCLOSURE: &str = "highlights-tester-options";

/// One descriptor per color row in Message Highlights. The `control_id` matches
/// the `color_editor` key the row hands to the widget, so a search hit and the
/// focused control agree.
const MENTION_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.mention-color",
    label: "Mentions",
    aliases: &["mention highlight", "ping"],
    control_id: "highlight-mention",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const THREAD_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.thread-color",
    label: "Reply threads",
    aliases: &["Reply Threads", "reply thread highlight"],
    control_id: "highlight-thread",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const WHISPER_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.whisper-color",
    label: "Whispers",
    aliases: &["whisper highlight", "direct message highlight"],
    control_id: "highlight-whisper",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const SUBSCRIPTION_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.subscription-color",
    label: "Subscriptions",
    aliases: &["subscription highlight", "resub", "gift subscription"],
    control_id: "highlight-subscription",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const FIRST_MESSAGE_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.first-message-color",
    label: "First message",
    aliases: &["First Message", "first-time chatter highlight"],
    control_id: "highlight-first_message",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const WATCHSTREAK_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.watchstreak-color",
    label: "Watch streak",
    aliases: &["Watch Streak", "watch streak highlight"],
    control_id: "highlight-watchstreak",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const AUTOMOD_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.automod-color",
    label: "AutoMod-held messages",
    aliases: &["AutoMod-held Messages", "automod highlight", "held message"],
    control_id: "highlight-automod",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const ANNOUNCEMENT_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.announcement-color",
    label: "Announcements",
    aliases: &["announcement highlight", "broadcast highlight"],
    control_id: "highlight-announcement",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const RAID_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.raid-color",
    label: "Raids",
    aliases: &["raid highlight"],
    control_id: "highlight-raid",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const BITS_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.bits-color",
    label: "Bits / cheers",
    aliases: &["Bits / Cheers", "bits highlight", "cheer highlight"],
    control_id: "highlight-bits",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const PHRASE_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.phrase-color",
    label: "Highlight phrases",
    aliases: &[
        "Highlight Phrases",
        "highlight phrase color",
        "keyword highlight",
    ],
    control_id: "highlight-phrase",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const REDEEM_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.redeem-color",
    label: "Channel point redeems",
    aliases: &[
        "Channel Point Redeems",
        "channel points",
        "redeem",
        "reward",
    ],
    control_id: "highlight-redeem",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const ELEVATED_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.elevated-color",
    label: "Elevated / Hype Chat",
    aliases: &["hype chat", "paid", "elevated"],
    control_id: "highlight-elevated",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const SELF_MESSAGE_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.self-message-color",
    label: "Your own messages",
    aliases: &["Your Own Messages", "own message highlight"],
    control_id: "highlight-self_message",
    disclosures: &[HIGHLIGHTS_COLORS_DISCLOSURE],
};

const QUICK_PRESETS: SettingDescriptor = SettingDescriptor {
    id: "highlights.quick-presets",
    label: "Quick presets",
    aliases: &["Quick Presets", "preset", "palette", "reset colors"],
    control_id: "highlight-quick-presets",
    disclosures: &[],
};

/// Highlights › Preview & colors.
pub const HIGHLIGHTS_PREVIEW_COLORS_SETTINGS: &[SettingDescriptor] = &[
    QUICK_PRESETS,
    MENTION_COLOR,
    THREAD_COLOR,
    WHISPER_COLOR,
    FIRST_MESSAGE_COLOR,
    SELF_MESSAGE_COLOR,
    PHRASE_COLOR,
    SUBSCRIPTION_COLOR,
    WATCHSTREAK_COLOR,
    ANNOUNCEMENT_COLOR,
    RAID_COLOR,
    BITS_COLOR,
    REDEEM_COLOR,
    ELEVATED_COLOR,
    AUTOMOD_COLOR,
];

const SELF_MESSAGE_HIGHLIGHT: SettingDescriptor = SettingDescriptor {
    id: "highlights.self-message-highlight",
    label: "Highlight your own messages",
    aliases: &["own-message highlight tint"],
    control_id: "self-message-highlight-enabled",
    disclosures: &[HIGHLIGHTS_BEHAVIOR_DISCLOSURE],
};

const ELEVATED_HIGHLIGHT: SettingDescriptor = SettingDescriptor {
    id: "highlights.elevated-highlight",
    label: "Highlight elevated / Hype Chat",
    aliases: &["hype chat", "paid", "elevated"],
    control_id: "elevated-highlight-enabled",
    disclosures: &[HIGHLIGHTS_BEHAVIOR_DISCLOSURE],
};

const THREAD_HIGHLIGHT: SettingDescriptor = SettingDescriptor {
    id: "highlights.thread-highlight",
    label: "Highlight participated threads",
    aliases: &["reply thread", "participated"],
    control_id: "thread-highlight-enabled",
    disclosures: &[],
};

const THREAD_HIGHLIGHT_MENTIONS: SettingDescriptor = SettingDescriptor {
    id: "highlights.thread-highlight-mentions",
    label: "Show reply threads in mentions",
    aliases: &["activity mentions", "reply thread"],
    control_id: "thread-highlight-mentions",
    disclosures: &[HIGHLIGHTS_BEHAVIOR_DISCLOSURE],
};

const THREAD_HIGHLIGHT_SOUND: SettingDescriptor = SettingDescriptor {
    id: "highlights.thread-highlight-sound",
    label: "Play reply-thread sound",
    aliases: &["reply-thread audio", "reply-thread mention audio"],
    control_id: "thread-highlight-sound",
    disclosures: &[HIGHLIGHTS_BEHAVIOR_DISCLOSURE],
};

const FLASH_ON_MENTION: SettingDescriptor = SettingDescriptor {
    id: "highlights.flash-on-mention",
    label: "Flash window on mention",
    aliases: &["taskbar", "flash", "attention"],
    control_id: "flash-mention",
    disclosures: &[],
};

const MENTION_IN_TITLE: SettingDescriptor = SettingDescriptor {
    id: "highlights.mention-in-title",
    label: "Show mention in title",
    aliases: &["window title", "titlebar"],
    control_id: "mention-title",
    disclosures: &[],
};

const HIGHLIGHT_FLASH: SettingDescriptor = SettingDescriptor {
    id: "appearance.highlight-flash",
    label: "Highlight flash",
    aliases: &["animation", "mention flash"],
    control_id: "highlight-flash",
    disclosures: &[HIGHLIGHTS_BEHAVIOR_DISCLOSURE],
};

/// Highlights › Behavior.
pub const HIGHLIGHTS_BEHAVIOR_SETTINGS: &[SettingDescriptor] = &[
    FLASH_ON_MENTION,
    MENTION_IN_TITLE,
    THREAD_HIGHLIGHT,
    SELF_MESSAGE_HIGHLIGHT,
    ELEVATED_HIGHLIGHT,
    THREAD_HIGHLIGHT_MENTIONS,
    THREAD_HIGHLIGHT_SOUND,
    HIGHLIGHT_FLASH,
];

const CUSTOM_HIGHLIGHT_RULES: SettingDescriptor = SettingDescriptor {
    id: "highlights.custom-rules",
    label: "Custom highlight rules",
    aliases: &["custom highlight editor", "highlight rule editor"],
    control_id: "highlight-rule-pattern",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_BLACKLIST: SettingDescriptor = SettingDescriptor {
    id: "highlights.blacklist",
    label: "Ignored users / blacklist",
    aliases: &["highlight exclusion editor", "highlight blacklist"],
    control_id: "highlight-blacklist-pattern",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_TARGET: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-target",
    label: "Highlight rule target",
    aliases: &[
        "highlight target phrase",
        "highlight target user",
        "highlight target badge",
    ],
    control_id: "highlight-rule-target-phrase",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_COLOR: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-color",
    label: "Highlight rule color override",
    aliases: &["custom highlight tint", "rule color override"],
    control_id: "highlight-rule-color",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_SOUND_PATH: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-sound-path",
    label: "Highlight rule sound file",
    aliases: &["highlight rule sound file"],
    control_id: "highlight-rule-sound-path",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_REGEX: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-regex",
    label: "Highlight rule regex",
    aliases: &["highlight rule regular expression"],
    control_id: "highlight-rule-regex",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_CASE_SENSITIVE: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-case-sensitive",
    label: "Highlight rule case sensitive",
    aliases: &["highlight rule case matching"],
    control_id: "highlight-rule-case-sensitive",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_SHOW_IN_MENTIONS: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-show-in-mentions",
    label: "Show highlight rule in mentions",
    aliases: &["highlight rule activity mentions"],
    control_id: "highlight-rule-show-in-mentions",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_PLAY_SOUND: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-play-sound",
    label: "Play highlight rule sound",
    aliases: &["highlight rule audio"],
    control_id: "highlight-rule-play-sound",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_ALERT: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-alert",
    label: "Highlight rule alert",
    aliases: &["highlight rule notification"],
    control_id: "highlight-rule-alert",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_RULE_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "highlights.rule-enabled",
    label: "Highlight rule enabled",
    aliases: &["activate highlight rule"],
    control_id: "highlight-rule-enabled",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_BLACKLIST_REGEX: SettingDescriptor = SettingDescriptor {
    id: "highlights.blacklist-regex",
    label: "Ignored user regex",
    aliases: &["ignored user regular expression"],
    control_id: "highlight-blacklist-regex",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

const HIGHLIGHT_BLACKLIST_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "highlights.blacklist-enabled",
    label: "Ignored user enabled",
    aliases: &["activate ignored user rule"],
    control_id: "highlight-blacklist-enabled",
    disclosures: &[HIGHLIGHTS_RULES_DISCLOSURE],
};

/// Highlights › Custom rules.
pub const HIGHLIGHTS_CUSTOM_RULES_SETTINGS: &[SettingDescriptor] = &[
    HIGHLIGHT_RULE_TARGET,
    CUSTOM_HIGHLIGHT_RULES,
    HIGHLIGHT_RULE_COLOR,
    HIGHLIGHT_RULE_SOUND_PATH,
    HIGHLIGHT_RULE_REGEX,
    HIGHLIGHT_RULE_CASE_SENSITIVE,
    HIGHLIGHT_RULE_SHOW_IN_MENTIONS,
    HIGHLIGHT_RULE_PLAY_SOUND,
    HIGHLIGHT_RULE_ALERT,
    HIGHLIGHT_RULE_ENABLED,
    HIGHLIGHT_BLACKLIST,
    HIGHLIGHT_BLACKLIST_REGEX,
    HIGHLIGHT_BLACKLIST_ENABLED,
    HIGHLIGHT_TESTER_MESSAGE,
    HIGHLIGHT_TESTER_SENDER,
    HIGHLIGHT_TESTER_BADGES,
    HIGHLIGHT_TESTER_CHANNEL,
    HIGHLIGHT_TESTER_SENT_BY_ME,
    HIGHLIGHT_TESTER_INCLUDE_DRAFT,
];

const HIGHLIGHT_TESTER_MESSAGE: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-message",
    label: "Test your highlights",
    aliases: &[
        "highlight tester",
        "test highlight",
        "sample message",
        "why is this highlighted",
    ],
    control_id: "highlight-tester-message",
    disclosures: &[],
};

const HIGHLIGHT_TESTER_SENDER: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-sender",
    label: "Test sender",
    aliases: &["sample sender", "highlight test username"],
    control_id: "highlight-tester-sender",
    disclosures: &[],
};

const HIGHLIGHT_TESTER_BADGES: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-badges",
    label: "Test badges",
    aliases: &["sample badges", "highlight test badge"],
    control_id: "highlight-tester-badges",
    disclosures: &[HIGHLIGHT_TESTER_DISCLOSURE],
};

const HIGHLIGHT_TESTER_CHANNEL: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-channel",
    label: "Test channel",
    aliases: &["sample channel", "highlight test channel"],
    control_id: "highlight-tester-channel",
    disclosures: &[HIGHLIGHT_TESTER_DISCLOSURE],
};

const HIGHLIGHT_TESTER_SENT_BY_ME: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-sent-by-me",
    label: "Sent by me",
    aliases: &["test own message", "highlight test self"],
    control_id: "highlight-tester-sent-by-me",
    disclosures: &[HIGHLIGHT_TESTER_DISCLOSURE],
};

const HIGHLIGHT_TESTER_INCLUDE_DRAFT: SettingDescriptor = SettingDescriptor {
    id: "highlights.tester-include-draft",
    label: "Include the rule I'm editing",
    aliases: &["test unsaved rule", "test draft rule"],
    control_id: "highlight-tester-include-draft",
    disclosures: &[HIGHLIGHT_TESTER_DISCLOSURE],
};

/// Get highlight settings from backend
fn get_highlight_settings() -> HighlightSettings {
    let local = backend::get_local_settings();
    HighlightSettings {
        mention_color: local.mention_color,
        whisper_color: local.whisper_color,
        subscription_color: local.subscription_color,
        first_message_color: local.first_message_color,
        thread_highlight_color: local.thread_highlight_color,
        watchstreak_color: crate::backend::settings::get_watchstreak_highlight_color(),
        automod_color: crate::backend::settings::get_automod_highlight_color(),
        announcement_color: crate::backend::settings::get_announcement_highlight_color(),
        raid_color: crate::backend::settings::get_raid_highlight_color(),
        bits_color: crate::backend::settings::get_bits_highlight_color(),
        phrase_color: crate::backend::settings::get_phrase_highlight_color(),
        redeem_color: crate::backend::settings::get_redeem_highlight_color(),
        elevated_color: crate::backend::settings::get_elevated_highlight_color(),
        self_message_color: crate::backend::settings::get_self_message_highlight_color(),
        enable_self_message_highlight: crate::backend::settings::get_enable_self_message_highlight(
        ),
        enable_elevated_highlight: crate::backend::settings::get_enable_elevated_highlight(),
        flash_on_mention: local.flash_on_mention,
        show_mention_in_title: local.show_mention_in_title,
        thread_highlight_enabled: local.thread_highlight_enabled,
        thread_highlight_sound_enabled: local.thread_highlight_sound_enabled,
        thread_highlight_show_in_mentions: local.thread_highlight_show_in_mentions,
    }
}

#[derive(Clone)]
struct HighlightSettings {
    mention_color: String,
    whisper_color: String,
    subscription_color: String,
    first_message_color: String,
    thread_highlight_color: String,
    watchstreak_color: String,
    automod_color: String,
    announcement_color: String,
    raid_color: String,
    bits_color: String,
    phrase_color: String,
    redeem_color: String,
    elevated_color: String,
    self_message_color: String,
    enable_self_message_highlight: bool,
    enable_elevated_highlight: bool,
    flash_on_mention: bool,
    show_mention_in_title: bool,
    thread_highlight_enabled: bool,
    thread_highlight_sound_enabled: bool,
    thread_highlight_show_in_mentions: bool,
}

impl Default for HighlightSettings {
    fn default() -> Self {
        Self {
            mention_color: backend::settings::DEFAULT_MENTION_COLOR.to_string(),
            whisper_color: backend::settings::DEFAULT_WHISPER_COLOR.to_string(),
            subscription_color: backend::settings::DEFAULT_SUBSCRIPTION_COLOR.to_string(),
            first_message_color: backend::settings::DEFAULT_FIRST_MESSAGE_COLOR.to_string(),
            thread_highlight_color: backend::settings::DEFAULT_THREAD_HIGHLIGHT_COLOR.to_string(),
            watchstreak_color: backend::settings::DEFAULT_WATCHSTREAK_HIGHLIGHT_COLOR.to_string(),
            automod_color: backend::settings::DEFAULT_AUTOMOD_HIGHLIGHT_COLOR.to_string(),
            announcement_color: backend::settings::DEFAULT_ANNOUNCEMENT_HIGHLIGHT_COLOR.to_string(),
            raid_color: backend::settings::DEFAULT_RAID_HIGHLIGHT_COLOR.to_string(),
            bits_color: backend::settings::DEFAULT_BITS_HIGHLIGHT_COLOR.to_string(),
            phrase_color: backend::settings::DEFAULT_PHRASE_HIGHLIGHT_COLOR.to_string(),
            redeem_color: backend::settings::DEFAULT_REDEEM_HIGHLIGHT_COLOR.to_string(),
            elevated_color: backend::settings::DEFAULT_ELEVATED_HIGHLIGHT_COLOR.to_string(),
            self_message_color: backend::settings::DEFAULT_SELF_MESSAGE_HIGHLIGHT_COLOR.to_string(),
            enable_self_message_highlight: settings_defaults().enable_self_message_highlight,
            enable_elevated_highlight: settings_defaults().enable_elevated_highlight,
            flash_on_mention: settings_defaults().flash_on_mention,
            show_mention_in_title: settings_defaults().show_mention_in_title,
            thread_highlight_enabled: settings_defaults().thread_highlight_enabled,
            thread_highlight_sound_enabled: settings_defaults().thread_highlight_sound_enabled,
            thread_highlight_show_in_mentions: settings_defaults()
                .thread_highlight_show_in_mentions,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HighlightBlacklistEditorField {
    Pattern,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HighlightRuleEditorField {
    Pattern,
    SoundPath,
}

#[derive(Clone)]
struct HighlightBlacklistEditorState {
    pattern: String,
    editing_id: Option<String>,
    regex: bool,
    enabled: bool,
    status: Option<String>,
    status_is_error: bool,
    focused: Option<HighlightBlacklistEditorField>,
}

impl Default for HighlightBlacklistEditorState {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            editing_id: None,
            regex: false,
            enabled: true,
            status: None,
            status_is_error: false,
            focused: None,
        }
    }
}

#[derive(Clone)]
struct HighlightRuleEditorState {
    target: HighlightRuleTarget,
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    show_in_mentions: bool,
    play_sound: bool,
    alert: bool,
    enabled: bool,
    color: String,
    sound_path: String,
    editing_id: Option<String>,
    status: Option<String>,
    status_is_error: bool,
    focused: Option<HighlightRuleEditorField>,
}

impl Default for HighlightRuleEditorState {
    fn default() -> Self {
        Self {
            target: HighlightRuleTarget::Phrase,
            pattern: String::new(),
            regex: false,
            case_sensitive: false,
            show_in_mentions: true,
            play_sound: false,
            alert: false,
            enabled: true,
            color: String::new(),
            sound_path: String::new(),
            editing_id: None,
            status: None,
            status_is_error: false,
            focused: None,
        }
    }
}

static HIGHLIGHT_BLACKLIST_EDITOR: LazyLock<EditorState<HighlightBlacklistEditorState>> =
    LazyLock::new(|| {
        EditorState::new(
            "highlight blacklist",
            HighlightBlacklistEditorState::default(),
        )
    });
static HIGHLIGHT_RULE_EDITOR: LazyLock<EditorState<HighlightRuleEditorState>> =
    LazyLock::new(|| EditorState::new("highlight rules", HighlightRuleEditorState::default()));
static HIGHLIGHT_SETTINGS_DEFAULTS: LazyLock<HighlightSettings> =
    LazyLock::new(HighlightSettings::default);

/// The highlight tester's inputs and last result. Never persisted; samples
/// are only evaluated, never added to chat.
#[derive(Clone, Default)]
struct HighlightTesterState {
    /// `None` until edited, so the default follows the signed-in login.
    message: Option<String>,
    sender: Option<String>,
    badges: String,
    channel: String,
    sent_by_me: bool,
    include_draft: bool,
    result: Option<HighlightTesterResult>,
}

#[derive(Clone)]
struct HighlightTesterResult {
    message: String,
    outcome: HighlightTestOutcome,
    draft_notes: Vec<&'static str>,
}

impl HighlightTesterState {
    fn message_text(&self) -> String {
        self.message.clone().unwrap_or_else(|| {
            let login = backend::settings::get_username()
                .map(|login| login.trim().to_string())
                .filter(|login| !login.is_empty())
                .unwrap_or_else(|| "someone".to_string());
            format!("hey @{login}, check this")
        })
    }

    fn sender_text(&self) -> String {
        self.sender
            .clone()
            .unwrap_or_else(|| "test_viewer".to_string())
    }
}

static HIGHLIGHT_TESTER: LazyLock<EditorState<HighlightTesterState>> =
    LazyLock::new(|| EditorState::new("highlight tester", HighlightTesterState::default()));

/// Render the Highlights page as its three compatible scroll anchors.
pub fn render_highlights_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let settings = get_highlight_settings();
    let highlight_rules = get_highlight_rules();
    let highlight_rule_editor = highlight_rule_editor_snapshot_from(&highlight_rules);
    let blacklist_users = get_highlight_blacklist_users();
    let blacklist_editor = highlight_blacklist_editor_snapshot_from(&blacklist_users);

    SettingsPage::new(SettingsSectionId::Highlights)
        .subsection(
            SettingsSubsectionId::HighlightsBehavior,
            render_highlights_attention(&settings, ui, cx),
        )
        .subsection(
            SettingsSubsectionId::HighlightsCustomRules,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(SettingsMetrics::GROUP_GAP))
                .child(render_highlights_rules(
                    highlight_rules,
                    highlight_rule_editor,
                    blacklist_users,
                    blacklist_editor,
                    ui,
                    window,
                    cx,
                ))
                .child(render_highlight_tester(ui, window, cx)),
        )
        .subsection(
            SettingsSubsectionId::HighlightsPreviewColors,
            render_highlights_palette(&settings, ui, cx),
        )
}

fn render_highlights_attention<T: 'static>(
    settings: &HighlightSettings,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = &*HIGHLIGHT_SETTINGS_DEFAULTS;
    let app_defaults = settings_defaults();
    let disclosure = ui.disclosure(HIGHLIGHTS_BEHAVIOR_DISCLOSURE, cx);
    let behavior_modified = usize::from(
        settings.enable_self_message_highlight != defaults.enable_self_message_highlight,
    ) + usize::from(
        settings.enable_elevated_highlight != defaults.enable_elevated_highlight,
    ) + usize::from(
        settings.thread_highlight_show_in_mentions != defaults.thread_highlight_show_in_mentions,
    ) + usize::from(
        settings.thread_highlight_sound_enabled != defaults.thread_highlight_sound_enabled,
    ) + usize::from(
        crate::backend::settings::get_highlight_flash_enabled()
            != app_defaults.highlight_flash_enabled,
    );

    settings_group(
        "highlights-attention",
        "Attention",
        false,
        settings_group_contents(
            "highlights-attention-rows",
            "Attention",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(highlight_bool_row(
                    FLASH_ON_MENTION,
                    settings.flash_on_mention,
                    defaults.flash_on_mention,
                    "Flash the taskbar when someone mentions you.",
                    ui.stacked,
                    cx,
                    backend::set_flash_on_mention,
                ))
                .child(highlight_bool_row(
                    MENTION_IN_TITLE,
                    settings.show_mention_in_title,
                    defaults.show_mention_in_title,
                    "Show a mention indicator in the window title.",
                    ui.stacked,
                    cx,
                    backend::set_show_mention_in_title,
                ))
                .child(highlight_bool_row(
                    THREAD_HIGHLIGHT,
                    settings.thread_highlight_enabled,
                    defaults.thread_highlight_enabled,
                    "Highlight replies in threads you participated in.",
                    ui.stacked,
                    cx,
                    backend::set_thread_highlight_enabled,
                ))
                .child(settings_disclosure_with_layout(
                    HIGHLIGHTS_BEHAVIOR_DISCLOSURE,
                    "Highlight behavior",
                    Some("Your messages, Hype Chat, reply threads, and flash"),
                    behavior_modified,
                    &disclosure,
                    ui.stacked,
                    cx,
                    move |cx| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(Spacing::SM))
                            .child(settings_subtopic(
                                "highlights-behavior-messages",
                                "Messages",
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(highlight_bool_row(
                                        SELF_MESSAGE_HIGHLIGHT,
                                        settings.enable_self_message_highlight,
                                        defaults.enable_self_message_highlight,
                                        "Tint messages you send with your own-message color.",
                                        ui.stacked,
                                        cx,
                                        crate::backend::settings::set_enable_self_message_highlight,
                                    ))
                                    .child(highlight_bool_row(
                                        ELEVATED_HIGHLIGHT,
                                        settings.enable_elevated_highlight,
                                        defaults.enable_elevated_highlight,
                                        "Tint paid elevated and Hype Chat messages.",
                                        ui.stacked,
                                        cx,
                                        crate::backend::settings::set_enable_elevated_highlight,
                                    )),
                            ))
                            .child(settings_subtopic(
                                "highlights-behavior-threads",
                                "Reply threads",
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(highlight_bool_row(
                                        THREAD_HIGHLIGHT_MENTIONS,
                                        settings.thread_highlight_show_in_mentions,
                                        defaults.thread_highlight_show_in_mentions,
                                        "Include participated reply threads in Activity Mentions.",
                                        ui.stacked,
                                        cx,
                                        backend::set_thread_highlight_show_in_mentions,
                                    ))
                                    .child(highlight_bool_row(
                                        THREAD_HIGHLIGHT_SOUND,
                                        settings.thread_highlight_sound_enabled,
                                        defaults.thread_highlight_sound_enabled,
                                        "Use the mention sound for participated reply threads.",
                                        ui.stacked,
                                        cx,
                                        backend::set_thread_highlight_sound_enabled,
                                    )),
                            ))
                            .child(settings_subtopic(
                                "highlights-behavior-effects",
                                "Effects",
                                highlight_bool_row(
                                    HIGHLIGHT_FLASH,
                                    crate::backend::settings::get_highlight_flash_enabled(),
                                    app_defaults.highlight_flash_enabled,
                                    "Flash on mentions and highlighted messages.",
                                    ui.stacked,
                                    cx,
                                    crate::backend::settings::set_highlight_flash_enabled,
                                ),
                            ))
                    },
                )),
        ),
    )
}

fn highlight_bool_row<T: 'static, F>(
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
    let modified = value != default;
    let reset = set.clone();
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
                setting_reset_with_window(
                    format!("reset-{}", descriptor.control_id),
                    descriptor.label,
                    cx,
                    move |_, cx| {
                        reset(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

fn render_highlights_palette<T: 'static>(
    settings: &HighlightSettings,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let colors_modified = highlight_color_modified_count(settings);
    let disclosure = ui.disclosure(HIGHLIGHTS_COLORS_DISCLOSURE, cx);
    let preset_select = ui.select(QUICK_PRESETS.control_id, cx);
    let presets = [
        SelectOption::new("default", "Default"),
        SelectOption::new("muted", "Muted"),
        SelectOption::new("vibrant", "Vibrant"),
        SelectOption::new("ocean", "Ocean"),
    ];

    settings_group(
        "highlights-palette",
        "Palette",
        false,
        settings_group_contents(
            "highlights-palette-rows",
            "Palette",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap(px(Spacing::MD))
                        .map(|row| {
                            if ui.stacked {
                                row.flex_col()
                            } else {
                                row.items_start()
                            }
                        })
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(super::theme::render_highlight_preview()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .map(|control| {
                                    if ui.stacked {
                                        control.w_full()
                                    } else {
                                        control.w(px(200.0))
                                    }
                                })
                                .flex()
                                .flex_col()
                                .gap(px(Spacing::XS))
                                .child(setting_target(
                                    QUICK_PRESETS,
                                    settings_menu_button(
                                        QUICK_PRESETS.control_id,
                                        "Apply preset",
                                        &presets,
                                        &preset_select,
                                        cx,
                                        |value, cx| {
                                            apply_highlight_color_preset(&value);
                                            cx.notify();
                                        },
                                    ),
                                )),
                        ),
                )
                .child(settings_disclosure_with_layout(
                    HIGHLIGHTS_COLORS_DISCLOSURE,
                    "Highlight colors",
                    Some("Messages, events, and moderation"),
                    colors_modified,
                    &disclosure,
                    ui.stacked,
                    cx,
                    move |cx| render_highlight_colors(settings, ui.stacked, cx),
                )),
        ),
    )
}

fn render_highlight_colors<T: 'static>(
    settings: &HighlightSettings,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(settings_subtopic(
            "highlight-colors-messages",
            "Messages",
            div()
                .flex()
                .flex_col()
                .child(render_highlight_row(
                    MENTION_COLOR,
                    &settings.mention_color,
                    "mention",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    THREAD_COLOR,
                    &settings.thread_highlight_color,
                    "thread",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    WHISPER_COLOR,
                    &settings.whisper_color,
                    "whisper",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    FIRST_MESSAGE_COLOR,
                    &settings.first_message_color,
                    "first_message",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    SELF_MESSAGE_COLOR,
                    &settings.self_message_color,
                    "self_message",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    PHRASE_COLOR,
                    &settings.phrase_color,
                    "phrase",
                    stacked,
                    cx,
                )),
        ))
        .child(settings_subtopic(
            "highlight-colors-events",
            "Events",
            div()
                .flex()
                .flex_col()
                .child(render_highlight_row(
                    SUBSCRIPTION_COLOR,
                    &settings.subscription_color,
                    "subscription",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    WATCHSTREAK_COLOR,
                    &settings.watchstreak_color,
                    "watchstreak",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    ANNOUNCEMENT_COLOR,
                    &settings.announcement_color,
                    "announcement",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    RAID_COLOR,
                    &settings.raid_color,
                    "raid",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    BITS_COLOR,
                    &settings.bits_color,
                    "bits",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    REDEEM_COLOR,
                    &settings.redeem_color,
                    "redeem",
                    stacked,
                    cx,
                ))
                .child(render_highlight_row(
                    ELEVATED_COLOR,
                    &settings.elevated_color,
                    "elevated",
                    stacked,
                    cx,
                )),
        ))
        .child(settings_subtopic(
            "highlight-colors-moderation",
            "Moderation",
            render_highlight_row(
                AUTOMOD_COLOR,
                &settings.automod_color,
                "automod",
                stacked,
                cx,
            ),
        ))
}

fn render_highlights_rules<T: 'static>(
    highlight_rules: Vec<HighlightRule>,
    highlight_rule_editor: EditorSnapshot<HighlightRuleEditorState>,
    blacklist_users: Vec<HighlightBlacklistUser>,
    blacklist_editor: EditorSnapshot<HighlightBlacklistEditorState>,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rule_count = highlight_rules.len();
    let blacklist_count = blacklist_users.len();
    let summary = highlight_rules_summary(
        rule_count,
        &highlight_rule_editor,
        blacklist_count,
        &blacklist_editor,
    );
    let disclosure = ui.disclosure(HIGHLIGHTS_RULES_DISCLOSURE, cx);
    if !disclosure.is_expanded()
        && (highlight_rule_editor.draft.focused.is_some()
            || blacklist_editor.draft.focused.is_some())
    {
        update_highlight_rule_editor(|editor| editor.focused = None);
        update_highlight_blacklist_editor(|editor| editor.focused = None);
    }
    let fields = HighlightTextFields {
        rule_pattern: highlight_rule_input(
            CUSTOM_HIGHLIGHT_RULES.control_id,
            HighlightRuleEditorField::Pattern,
            &highlight_rule_editor.draft.pattern,
            "Pattern",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
        rule_sound_path: highlight_rule_input(
            "highlight-rule-sound-path",
            HighlightRuleEditorField::SoundPath,
            &highlight_rule_editor.draft.sound_path,
            "Optional sound file path",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
        blacklist_pattern: highlight_blacklist_input(
            HIGHLIGHT_BLACKLIST.control_id,
            &blacklist_editor.draft.pattern,
            "Username or pattern",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
    };

    settings_group(
        "highlights-rule-group",
        "Highlight rules",
        false,
        settings_summary_disclosure(
            HIGHLIGHTS_RULES_DISCLOSURE,
            "Highlight rules",
            &summary,
            &disclosure,
            cx,
            move |cx| {
                render_highlights_rules_body(
                    highlight_rules,
                    highlight_rule_editor,
                    blacklist_users,
                    blacklist_editor,
                    fields,
                    cx,
                )
            },
        ),
    )
}

/// Text inputs for the rule and blacklist editors, prepared while the window
/// is still available.
struct HighlightTextFields<T> {
    rule_pattern: DraftField<T>,
    rule_sound_path: DraftField<T>,
    blacklist_pattern: DraftField<T>,
}

fn render_highlights_rules_body<T: 'static>(
    highlight_rules: Vec<HighlightRule>,
    highlight_rule_editor: EditorSnapshot<HighlightRuleEditorState>,
    blacklist_users: Vec<HighlightBlacklistUser>,
    blacklist_editor: EditorSnapshot<HighlightBlacklistEditorState>,
    fields: HighlightTextFields<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let highlight_rule_generation = highlight_rule_editor.generation;
    let highlight_rule_needs_review = highlight_rule_editor.needs_review;
    let highlight_rule_editor = highlight_rule_editor.draft;
    let blacklist_generation = blacklist_editor.generation;
    let blacklist_needs_review = blacklist_editor.needs_review;
    let blacklist_editor = blacklist_editor.draft;
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(settings_subtopic(
            "highlights-rules-editor",
            "Rules",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .max_w(px(640.0))
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(
                            "Manage phrase, user, and badge highlight rules with optional sound files, alerts, mentions, and custom color overrides.",
                        ),
                )
                .child(render_highlight_rule_editor(
                    highlight_rule_editor,
                    highlight_rule_generation,
                    highlight_rule_needs_review,
                    fields.rule_pattern,
                    fields.rule_sound_path,
                    cx,
                ))
                .child(render_highlight_rule_list(highlight_rules, cx)),
        ))
        .child(settings_subtopic(
            "highlights-blacklist-editor",
            "Exclusions",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(
                            "Prevent selected usernames or patterns from triggering highlights.",
                        ),
                )
                .child(render_highlight_blacklist_editor(
                    blacklist_editor,
                    blacklist_generation,
                    blacklist_needs_review,
                    fields.blacklist_pattern,
                    cx,
                ))
                .child(render_highlight_blacklist_list(blacklist_users, cx)),
        ))
}

fn highlight_tester_snapshot() -> HighlightTesterState {
    HIGHLIGHT_TESTER
        .snapshot_or_reset(|_| HighlightTesterState::default())
        .draft
}

fn update_highlight_tester(update: impl FnOnce(&mut HighlightTesterState)) {
    HIGHLIGHT_TESTER.apply_user_edit(|_| HighlightTesterState::default(), update);
}

/// Run the tester once on its current inputs. Only Test and Enter call this,
/// never a keystroke.
fn run_highlight_test() {
    let tester = highlight_tester_snapshot();
    let (rules, draft_notes) = if tester.include_draft {
        highlight_rules_with_editor_draft()
    } else {
        (None, Vec::new())
    };
    let sample = HighlightSample {
        message: tester.message_text(),
        sender_login: tester.sender_text(),
        badges: tester
            .badges
            .split(',')
            .map(str::trim)
            .filter(|badge| !badge.is_empty())
            .map(str::to_string)
            .collect(),
        channel: tester.channel.clone(),
        sent_by_me: tester.sent_by_me,
        rules,
    };
    let outcome = test_highlight_sample(&sample);
    HIGHLIGHT_TESTER.apply_authoritative_update(
        |_| HighlightTesterState::default(),
        |tester| {
            tester.result = Some(HighlightTesterResult {
                message: sample.message,
                outcome,
                draft_notes,
            });
        },
    );
}

/// The saved rules with the rule editor's draft in place of the rule it
/// edits (or appended, for a new rule), as Save would store it. Returns `None`
/// with a note whenever Save would refuse the draft, and notes where the
/// tested rule differs from what was typed.
fn highlight_rules_with_editor_draft() -> (Option<Vec<HighlightRule>>, Vec<&'static str>) {
    let skipped = |note: &'static str| (None, vec![note]);
    let editor = highlight_rule_editor_snapshot().draft;
    let pattern = editor.pattern.trim();
    if pattern.is_empty() {
        return skipped("The rule editor is empty, so only saved rules were tested.");
    }
    if editor.regex && compile_highlight_regex(pattern, editor.case_sensitive).is_err() {
        return skipped(
            "The rule you're editing has an invalid regex, so only saved rules were tested.",
        );
    }
    let Ok(color) = normalize_highlight_rule_color(&editor.color) else {
        return skipped(
            "The rule you're editing has an invalid color, so only saved rules were tested.",
        );
    };
    let mut rules = get_highlight_rules();
    let editing = match editor.editing_id.as_deref() {
        Some(id) => match rules.iter().position(|rule| rule.id == id) {
            Some(index) => Some(index),
            None => {
                return skipped(
                    "The rule you were editing no longer exists, so only saved rules were tested.",
                );
            }
        },
        None => None,
    };

    let mut notes = Vec::new();
    // Save refuses a new network or device sound path: a new rule drops it and
    // an edited rule keeps its saved path. Either way it is never opened here.
    let mut sound_path = normalize_highlight_rule_sound_path(&editor.sound_path);
    let saved_sound_path = editing.and_then(|index| rules[index].sound_path.clone());
    if sound_path != saved_sound_path
        && sound_path
            .as_deref()
            .is_some_and(crate::backend::settings::is_remote_or_device_path)
    {
        notes.push(if saved_sound_path.is_some() {
            "Save keeps the rule's saved sound instead of a network path, so that sound was tested."
        } else {
            "Save refuses network sound paths, so the rule was tested without a sound file."
        });
        sound_path = saved_sound_path;
    }
    if !editor.enabled {
        notes.push("The rule you're editing is turned off, so it can't match.");
    }
    let draft = HighlightRule {
        id: editor.editing_id.clone().unwrap_or_default(),
        target: editor.target,
        pattern: pattern.to_string(),
        regex: editor.regex,
        case_sensitive: editor.case_sensitive,
        show_in_mentions: editor.show_in_mentions,
        play_sound: editor.play_sound,
        alert: editor.alert,
        sound_path,
        enabled: editor.enabled,
        color,
    };
    match editing {
        Some(index) => rules[index] = draft,
        None => rules.push(draft),
    }
    (Some(rules), notes)
}

fn play_highlight_test_sound(sound: &HighlightTestSound) {
    use crate::backend::sounds::{SOUND_STORE, SoundEventType};

    match sound {
        HighlightTestSound::Event(event) => SOUND_STORE.test_sound(*event),
        HighlightTestSound::RuleFile(path) => {
            SOUND_STORE.test_custom_sound(path, SoundEventType::Phrase);
        }
    }
}

fn render_highlight_tester<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let tester = highlight_tester_snapshot();
    let disclosure = ui.disclosure(HIGHLIGHT_TESTER_DISCLOSURE, cx);
    let stacked = ui.stacked;
    let message = DraftInput::new(
        HIGHLIGHT_TESTER_MESSAGE.control_id,
        &tester.message_text(),
        "Sample message",
    )
    .label("Test message")
    .on_change(|text, _cx| update_highlight_tester(|tester| tester.message = Some(text)))
    .on_enter(|_cx| run_highlight_test())
    .prepare(window, cx);
    let sender = DraftInput::new(
        HIGHLIGHT_TESTER_SENDER.control_id,
        &tester.sender_text(),
        "Sender login",
    )
    .label("Test sender login")
    .on_change(|text, _cx| update_highlight_tester(|tester| tester.sender = Some(text)))
    .on_enter(|_cx| run_highlight_test())
    .prepare(window, cx);
    let badges = DraftInput::new(
        HIGHLIGHT_TESTER_BADGES.control_id,
        &tester.badges,
        "moderator/1, subscriber/12",
    )
    .label("Test badges, comma separated")
    .monospace(true)
    .on_change(|text, _cx| update_highlight_tester(|tester| tester.badges = text))
    .on_enter(|_cx| run_highlight_test())
    .escape_focus(disclosure.clone())
    .prepare(window, cx);
    let channel = DraftInput::new(
        HIGHLIGHT_TESTER_CHANNEL.control_id,
        &tester.channel,
        "Channel (optional)",
    )
    .label("Test channel")
    .on_change(|text, _cx| update_highlight_tester(|tester| tester.channel = text))
    .on_enter(|_cx| run_highlight_test())
    .escape_focus(disclosure.clone())
    .prepare(window, cx);
    let options_modified = usize::from(!tester.badges.trim().is_empty())
        + usize::from(!tester.channel.trim().is_empty())
        + usize::from(tester.sent_by_me)
        + usize::from(tester.include_draft);
    let sent_by_me = tester.sent_by_me;
    let include_draft = tester.include_draft;
    let result = tester
        .result
        .map(|result| render_highlight_test_result(result, cx).into_any_element());

    settings_group(
        "highlights-tester",
        "Test your highlights",
        false,
        settings_group_contents(
            "highlights-tester-rows",
            "Test your highlights",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(
                    div()
                        .max_w(px(640.0))
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(
                            "Type a sample message to see what live chat would do with it. Samples are only evaluated: they never reach chat, history or Activity. Not simulated: replies, cheers, first messages, returning chatters, Elevated/Hype Chat, similar-message hiding and dimming, blocked senders, and sub-month filters.",
                        ),
                )
                .child(setting_target(
                    HIGHLIGHT_TESTER_MESSAGE,
                    highlight_tester_field("Message", message.render(cx)),
                ))
                .child(setting_target(
                    HIGHLIGHT_TESTER_SENDER,
                    highlight_tester_field("Sender", sender.render(cx)),
                ))
                .child(settings_disclosure_with_layout(
                    HIGHLIGHT_TESTER_DISCLOSURE,
                    "More test options",
                    Some("Badges, channel, and your own messages"),
                    options_modified,
                    &disclosure,
                    stacked,
                    cx,
                    move |cx| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(Spacing::SM))
                            .child(setting_target(
                                HIGHLIGHT_TESTER_BADGES,
                                highlight_tester_field("Badges", badges.render(cx)),
                            ))
                            .child(setting_target(
                                HIGHLIGHT_TESTER_CHANNEL,
                                highlight_tester_field("Channel", channel.render(cx)),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .items_center()
                                    .child(setting_target(
                                        HIGHLIGHT_TESTER_SENT_BY_ME,
                                        render_highlight_toggle_chip(
                                            HIGHLIGHT_TESTER_SENT_BY_ME.control_id.into(),
                                            "Sent by me",
                                            sent_by_me,
                                            |cx| {
                                                update_highlight_tester(|tester| {
                                                    tester.sent_by_me = !tester.sent_by_me;
                                                });
                                                cx.notify();
                                            },
                                            cx,
                                        ),
                                    ))
                                    .child(setting_target(
                                        HIGHLIGHT_TESTER_INCLUDE_DRAFT,
                                        render_highlight_toggle_chip(
                                            HIGHLIGHT_TESTER_INCLUDE_DRAFT.control_id.into(),
                                            "Include the rule I'm editing",
                                            include_draft,
                                            |cx| {
                                                update_highlight_tester(|tester| {
                                                    tester.include_draft = !tester.include_draft;
                                                });
                                                cx.notify();
                                            },
                                            cx,
                                        ),
                                    )),
                            )
                    },
                ))
                .child(
                    div().flex().items_center().gap_2().child(button_keyed(
                        "highlight-tester-run",
                        "Test",
                        true,
                        cx,
                        |cx| {
                            run_highlight_test();
                            cx.notify();
                        },
                    )),
                )
                .children(result),
        ),
    )
}

fn highlight_tester_field(label: &'static str, field: AnyElement) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(label),
        )
        .child(field)
}

fn render_highlight_test_result<T: 'static>(
    result: HighlightTesterResult,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let outcome = result.outcome;
    let explanation = outcome
        .highlight_type
        .as_ref()
        .map(echo_core::highlight_explanation::explain_highlight);
    let highlight = if outcome.hidden_by_filter {
        "Hidden by a filter".to_string()
    } else {
        explanation.as_ref().map_or_else(
            || "Not highlighted".to_string(),
            |explanation| explanation.kind.to_string(),
        )
    };
    let (why, why_detail) = match explanation {
        Some(explanation) => (explanation.title, explanation.detail),
        None if outcome.hidden_by_filter => (
            "A filter with the Hide action matched this message.".to_string(),
            None,
        ),
        None => (
            "Nothing in your highlight settings matches this message.".to_string(),
            None,
        ),
    };
    let mentions = if outcome.in_mentions {
        "Yes, it appears in Activity Mentions"
    } else {
        "No"
    };
    let play = outcome.sound.clone().map(|sound| {
        button_keyed(
            "highlight-tester-play-sound",
            "Play sound",
            false,
            cx,
            move |_cx| play_highlight_test_sound(&sound),
        )
    });
    let notes: Vec<String> = outcome
        .notes
        .into_iter()
        .chain(result.draft_notes.into_iter().map(str::to_string))
        .collect();

    div()
        .id("highlight-tester-result")
        .role(Role::Group)
        .aria_label("Highlight test result")
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(super::theme::render_highlight_sample_preview(
            "Test result",
            &outcome.sender_login,
            result.message,
            outcome.highlight_type.as_ref(),
        ))
        .child(highlight_test_line("Highlight", highlight, None))
        .child(highlight_test_line("Mentions", mentions.to_string(), None))
        .child(
            highlight_test_line("Sound", outcome.sound_summary, outcome.sound_note).children(play),
        )
        .child(highlight_test_line(
            "Desktop alert",
            outcome.alert_summary,
            outcome.alert_note,
        ))
        .child(highlight_test_line("Why", why, why_detail))
        .children(
            notes
                .into_iter()
                .map(|note| div().text_xs().text_color(colors::text_muted()).child(note)),
        )
}

fn highlight_test_line(label: &'static str, value: String, note: Option<String>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_start()
        .gap_2()
        .child(
            div()
                .flex_none()
                .w(px(104.0))
                .text_xs()
                .text_color(colors::text_muted())
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(Spacing::XXS))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_primary())
                        .child(value),
                )
                .when_some(note, |column, note| {
                    column.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child(note),
                    )
                }),
        )
}

fn render_highlight_rule_editor<T: 'static>(
    editor: HighlightRuleEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    pattern_input: DraftField<T>,
    sound_path_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editing = editor.editing_id.is_some();
    let custom_color = (!editor.color.trim().is_empty()).then_some(editor.color.as_str());

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(if editing {
                    "Edit Highlight Rule"
                } else {
                    "Add Highlight Rule"
                }),
        )
        .child(setting_target(
            HIGHLIGHT_RULE_TARGET,
            select_buttons(
                "highlight-rule-target",
                &[
                    SelectOption::new("phrase", "Phrase"),
                    SelectOption::new("user", "User"),
                    SelectOption::new("badge", "Badge"),
                ],
                highlight_rule_target_value(editor.target),
                cx,
                |value, cx| {
                    update_highlight_rule_editor(|editor| {
                        editor.target = match value.as_str() {
                            "user" => HighlightRuleTarget::User,
                            "badge" => HighlightRuleTarget::Badge,
                            _ => HighlightRuleTarget::Phrase,
                        };
                        editor.status = None;
                        editor.status_is_error = false;
                    });
                    cx.notify();
                },
            ),
        ))
        .child(setting_target(
            CUSTOM_HIGHLIGHT_RULES,
            pattern_input.render(cx),
        ))
        .child(setting_target(
            HIGHLIGHT_RULE_COLOR,
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Optional message tint override"),
                )
                .child(color_editor(
                    "highlight-rule-color",
                    custom_color,
                    ColorEditorOptions {
                        alpha_mode: ColorAlphaMode::Highlight {
                            default_alpha: (colors::highlight_alpha::PHRASE * 255.0).round() as u8,
                        },
                        presets: highlight_color_presets(),
                        allow_clear: true,
                        default_value: None,
                    },
                    cx,
                    |value, cx| {
                        update_highlight_rule_editor(|editor| {
                            editor.color = value.unwrap_or_default();
                            editor.status = None;
                            editor.status_is_error = false;
                        });
                        cx.notify();
                    },
                )),
        ))
        .child(setting_target(
            HIGHLIGHT_RULE_SOUND_PATH,
            sound_path_input.render(cx),
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .child(setting_target(
                    HIGHLIGHT_RULE_REGEX,
                    render_highlight_blacklist_toggle_chip(
                        "Regex",
                        editor.regex,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.regex = !editor.regex;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_RULE_CASE_SENSITIVE,
                    render_highlight_blacklist_toggle_chip(
                        "Case Sensitive",
                        editor.case_sensitive,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.case_sensitive = !editor.case_sensitive;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_RULE_SHOW_IN_MENTIONS,
                    render_highlight_blacklist_toggle_chip(
                        "Show in Mentions",
                        editor.show_in_mentions,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.show_in_mentions = !editor.show_in_mentions;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_RULE_PLAY_SOUND,
                    render_highlight_blacklist_toggle_chip(
                        "Play Sound",
                        editor.play_sound,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.play_sound = !editor.play_sound;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_RULE_ALERT,
                    render_highlight_blacklist_toggle_chip(
                        "Alert",
                        editor.alert,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.alert = !editor.alert;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_RULE_ENABLED,
                    render_highlight_blacklist_toggle_chip(
                        "Enabled",
                        editor.enabled,
                        |cx| {
                            update_highlight_rule_editor(|editor| {
                                editor.enabled = !editor.enabled;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                )),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(render_highlight_blacklist_button(
                    if editing { "Save" } else { "Add" },
                    true,
                    move |cx| {
                        save_highlight_rule_editor(editor_generation);
                        cx.notify();
                    },
                    cx,
                ))
                .when(editing, |row| {
                    row.child(render_highlight_blacklist_button(
                        "Cancel",
                        false,
                        |cx| {
                            reset_highlight_rule_editor();
                            cx.notify();
                        },
                        cx,
                    ))
                })
                .when_some(editor.status, |row, status| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(if editor.status_is_error || editor_needs_review {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        )
}

fn render_highlight_rule_list<T: 'static>(
    rules: Vec<HighlightRule>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(rules.is_empty(), |el| {
            el.child(empty_state("No custom highlight rules configured."))
        })
        .children(
            rules
                .into_iter()
                .map(|rule| render_highlight_rule_row(rule, cx)),
        )
}

fn render_highlight_rule_row<T: 'static>(
    rule: HighlightRule,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rule_for_toggle = rule.clone();
    let rule_for_edit = rule.clone();
    let rule_id_for_delete = rule.id.clone();
    let toggle_label = format!("Enable highlight rule {}", rule.pattern);
    let color_swatch = rule
        .color
        .as_ref()
        .filter(|color| is_valid_highlight_rule_color(color))
        .and_then(|color| colors::hex_to_hsla_argb(color, colors::highlight_alpha::PHRASE));

    div()
        .w_full()
        .p_2()
        .bg(colors::bg_surface())
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_start()
        .gap_3()
        .child(labelled_toggle(
            SharedString::from(format!("highlight-rule-toggle-{rule_id_for_delete}")),
            &toggle_label,
            rule.enabled,
            cx,
            move |enabled, cx| {
                let mut updated = rule_for_toggle.clone();
                updated.enabled = enabled;
                update_highlight_rule(&updated.id.clone(), updated);
                cx.notify();
            },
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_family("monospace")
                                .text_color(if rule.enabled {
                                    colors::text_primary()
                                } else {
                                    colors::text_muted()
                                })
                                .child(rule.pattern.clone()),
                        )
                        .when_some(color_swatch, |row, color| {
                            row.child(
                                div()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .rounded(Radii::sm())
                                    .bg(color)
                                    .border_1()
                                    .border_color(colors::border()),
                            )
                        })
                        .when_some(rule.color.clone(), |row, color| {
                            row.child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(colors::text_muted())
                                    .child(color),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .items_center()
                        .child(render_highlight_blacklist_mode_chip(
                            highlight_rule_target_label(&rule.target),
                            true,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.regex { "Regex" } else { "Literal" },
                            rule.regex,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.case_sensitive {
                                "Case Sensitive"
                            } else {
                                "Case Insensitive"
                            },
                            rule.case_sensitive,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.show_in_mentions {
                                "Mentions"
                            } else {
                                "No Mentions"
                            },
                            rule.show_in_mentions,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.play_sound { "Sound" } else { "Silent" },
                            rule.play_sound,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.alert { "Alert" } else { "No Alert" },
                            rule.alert,
                        ))
                        .when_some(rule.sound_path.clone(), |chips, sound_path| {
                            chips.child(render_highlight_blacklist_mode_chip(
                                if sound_path.trim().is_empty() {
                                    "Default Sound"
                                } else {
                                    "Custom Sound"
                                },
                                !sound_path.trim().is_empty(),
                            ))
                        })
                        .child(render_highlight_blacklist_mode_chip(
                            if rule.enabled { "Enabled" } else { "Disabled" },
                            rule.enabled,
                        )),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(render_highlight_blacklist_button(
                    "Edit",
                    false,
                    move |cx| {
                        set_highlight_rule_editor_for_rule(rule_for_edit.clone());
                        cx.notify();
                    },
                    cx,
                ))
                .child(action_button_keyed(
                    format!("delete-highlight-rule-{rule_id_for_delete}"),
                    "Delete",
                    true,
                    cx,
                    move |cx| {
                        remove_highlight_rule(&rule_id_for_delete);
                        if highlight_rule_editor_snapshot().draft.editing_id.as_deref()
                            == Some(rule_id_for_delete.as_str())
                        {
                            reset_highlight_rule_editor();
                        }
                        cx.notify();
                    },
                )),
        )
}

fn highlight_rule_target_value(target: HighlightRuleTarget) -> &'static str {
    match target {
        HighlightRuleTarget::Phrase => "phrase",
        HighlightRuleTarget::User => "user",
        HighlightRuleTarget::Badge => "badge",
    }
}

fn highlight_rule_input<T: 'static>(
    key: &'static str,
    field: HighlightRuleEditorField,
    value: &str,
    placeholder: &'static str,
) -> DraftInput<T> {
    DraftInput::new(key, value, placeholder)
        .monospace(true)
        .on_focus(move |_cx| {
            update_highlight_rule_editor(|editor| editor.focused = Some(field));
            update_highlight_blacklist_editor(|editor| editor.focused = None);
        })
        .on_blur(move |_cx| {
            update_highlight_rule_editor(|editor| {
                if editor.focused == Some(field) {
                    editor.focused = None;
                }
            });
        })
        .on_change(move |text, _cx| {
            update_highlight_rule_editor(|editor| {
                match field {
                    HighlightRuleEditorField::Pattern => editor.pattern = text,
                    HighlightRuleEditorField::SoundPath => editor.sound_path = text,
                }
                editor.status = None;
                editor.status_is_error = false;
            });
        })
        .on_enter(|_cx| {
            if update_highlight_rule_editor(|editor| editor.focused = None)
                == EditorEditResult::Applied
            {
                save_highlight_rule_editor(highlight_rule_editor_snapshot().generation);
            }
        })
        .on_escape(|_window, _cx| {
            update_highlight_rule_editor(|editor| editor.focused = None);
        })
}

fn highlight_rule_target_label(target: &HighlightRuleTarget) -> &'static str {
    match target {
        HighlightRuleTarget::Phrase => "Phrase",
        HighlightRuleTarget::User => "User",
        HighlightRuleTarget::Badge => "Badge",
    }
}

fn normalize_highlight_rule_color(color: &str) -> Result<Option<String>, &'static str> {
    let trimmed = color.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if !is_valid_highlight_rule_color(trimmed) {
        return Err("Choose a valid color");
    }

    Ok(Some(trimmed.to_string()))
}

fn normalize_highlight_rule_sound_path(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_matches('"');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_valid_highlight_rule_color(color: &str) -> bool {
    crate::backend::settings::hex_to_rgba(color).is_some()
}

fn highlight_rules_summary(
    rule_count: usize,
    rule_editor: &EditorSnapshot<HighlightRuleEditorState>,
    blacklist_count: usize,
    blacklist_editor: &EditorSnapshot<HighlightBlacklistEditorState>,
) -> String {
    let rules = match rule_count {
        1 => "1 rule".to_string(),
        count => format!("{count} rules"),
    };
    let exclusions = match blacklist_count {
        1 => "1 exclusion".to_string(),
        count => format!("{count} exclusions"),
    };
    let rule_draft = &rule_editor.draft;
    let blacklist_draft = &blacklist_editor.draft;
    let editing = rule_draft.editing_id.is_some() || blacklist_draft.editing_id.is_some();
    let needs_review = rule_editor.needs_review || blacklist_editor.needs_review;
    let has_draft = !rule_draft.pattern.is_empty()
        || !rule_draft.color.trim().is_empty()
        || !rule_draft.sound_path.trim().is_empty()
        || rule_draft.target != HighlightRuleTarget::Phrase
        || rule_draft.regex
        || rule_draft.case_sensitive
        || !rule_draft.show_in_mentions
        || rule_draft.play_sound
        || rule_draft.alert
        || !rule_draft.enabled
        || !blacklist_draft.pattern.is_empty()
        || blacklist_draft.regex
        || !blacklist_draft.enabled;
    if editing {
        format!("{rules} · {exclusions} · Editing…")
    } else if needs_review {
        format!("{rules} · {exclusions} · Needs review")
    } else if has_draft {
        format!("{rules} · {exclusions} · Unsaved draft")
    } else {
        format!("{rules} · {exclusions}")
    }
}

fn highlight_color_modified_count(settings: &HighlightSettings) -> usize {
    [
        ("mention", settings.mention_color.as_str()),
        ("thread", settings.thread_highlight_color.as_str()),
        ("whisper", settings.whisper_color.as_str()),
        ("subscription", settings.subscription_color.as_str()),
        ("first_message", settings.first_message_color.as_str()),
        ("watchstreak", settings.watchstreak_color.as_str()),
        ("automod", settings.automod_color.as_str()),
        ("announcement", settings.announcement_color.as_str()),
        ("raid", settings.raid_color.as_str()),
        ("bits", settings.bits_color.as_str()),
        ("phrase", settings.phrase_color.as_str()),
        ("redeem", settings.redeem_color.as_str()),
        ("elevated", settings.elevated_color.as_str()),
        ("self_message", settings.self_message_color.as_str()),
    ]
    .into_iter()
    .filter(|(kind, value)| {
        !setting_colors_match(
            value,
            highlight_default_value(kind),
            ColorAlphaMode::Highlight {
                default_alpha: highlight_default_alpha(kind),
            },
        )
    })
    .count()
}

fn render_highlight_row<T: 'static>(
    descriptor: SettingDescriptor,
    color_hex: &str,
    color_type: &str,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let default_alpha = highlight_default_alpha(color_type);
    let alpha_mode = ColorAlphaMode::Highlight { default_alpha };
    let default_value = highlight_default_value(color_type);
    let modified = !setting_colors_match(color_hex, default_value, alpha_mode);
    let color_type_owned = color_type.to_string();
    let picker_id = format!("highlight-{color_type}");
    let reset_picker_id = picker_id.clone();
    let reset_color_type = color_type.to_string();

    setting_target(
        descriptor,
        color_setting_row(
            descriptor.id,
            descriptor.label,
            None,
            &picker_id,
            Some(color_hex),
            ColorEditorOptions {
                alpha_mode,
                presets: highlight_color_presets(),
                allow_clear: false,
                default_value: Some(default_value),
            },
            modified,
            modified.then(|| {
                setting_reset_with_window(
                    format!("reset-{}", descriptor.control_id),
                    descriptor.label,
                    cx,
                    move |window, cx| {
                        set_highlight_color(&reset_color_type, default_value);
                        reset_color_editor_draft(
                            &reset_picker_id,
                            Some(default_value),
                            Some(default_value),
                            alpha_mode,
                            window,
                            cx,
                        );
                        cx.notify();
                    },
                )
            }),
            stacked,
            cx,
            move |value, cx| {
                if let Some(value) = value {
                    set_highlight_color(&color_type_owned, &value);
                }
                cx.notify();
            },
        ),
    )
}

fn apply_highlight_color_preset(name: &str) {
    match name {
        "default" => {
            backend::set_mention_color(backend::settings::DEFAULT_MENTION_COLOR);
            backend::set_whisper_color(backend::settings::DEFAULT_WHISPER_COLOR);
            backend::set_subscription_color(backend::settings::DEFAULT_SUBSCRIPTION_COLOR);
            backend::set_first_message_color(backend::settings::DEFAULT_FIRST_MESSAGE_COLOR);
            backend::set_thread_highlight_color(backend::settings::DEFAULT_THREAD_HIGHLIGHT_COLOR);
            crate::backend::settings::set_watchstreak_highlight_color(
                backend::settings::DEFAULT_WATCHSTREAK_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_automod_highlight_color(
                backend::settings::DEFAULT_AUTOMOD_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_announcement_highlight_color(
                backend::settings::DEFAULT_ANNOUNCEMENT_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_raid_highlight_color(
                backend::settings::DEFAULT_RAID_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_bits_highlight_color(
                backend::settings::DEFAULT_BITS_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_phrase_highlight_color(
                backend::settings::DEFAULT_PHRASE_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_redeem_highlight_color(
                backend::settings::DEFAULT_REDEEM_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_elevated_highlight_color(
                backend::settings::DEFAULT_ELEVATED_HIGHLIGHT_COLOR.to_string(),
            );
            crate::backend::settings::set_self_message_highlight_color(
                backend::settings::DEFAULT_SELF_MESSAGE_HIGHLIGHT_COLOR.to_string(),
            );
        }
        "muted" => {
            backend::set_mention_color("#666666");
            backend::set_whisper_color("#555555");
            backend::set_subscription_color("#444444");
            backend::set_first_message_color("#555555");
            backend::set_thread_highlight_color("#5f514c");
            crate::backend::settings::set_watchstreak_highlight_color("#666666".to_string());
            crate::backend::settings::set_automod_highlight_color("#555555".to_string());
        }
        "vibrant" => {
            backend::set_mention_color("#ff0000");
            backend::set_whisper_color("#ff00ff");
            backend::set_subscription_color("#00ff00");
            backend::set_first_message_color("#ffff00");
            backend::set_thread_highlight_color("#ff7a1a");
            crate::backend::settings::set_watchstreak_highlight_color("#ffcc00".to_string());
            crate::backend::settings::set_automod_highlight_color("#6b7280".to_string());
        }
        "ocean" => {
            backend::set_mention_color("#00bcd4");
            backend::set_whisper_color("#3f51b5");
            backend::set_subscription_color("#009688");
            backend::set_first_message_color("#4fc3f7");
            backend::set_thread_highlight_color("#0f766e");
            crate::backend::settings::set_watchstreak_highlight_color("#38bdf8".to_string());
            crate::backend::settings::set_automod_highlight_color("#475569".to_string());
        }
        _ => {}
    }
}

const HIGHLIGHT_COLOR_PRESETS: [(&str, &str); 8] = [
    ("Coral", "#FF6B6B"),
    ("Orange", "#F97316"),
    ("Gold", "#EAB308"),
    ("Green", "#22C55E"),
    ("Cyan", "#06B6D4"),
    ("Blue", "#3B82F6"),
    ("Violet", "#A855F7"),
    ("Pink", "#EC4899"),
];

fn highlight_color_presets() -> &'static [(&'static str, &'static str)] {
    &HIGHLIGHT_COLOR_PRESETS
}

fn highlight_default_value(color_type: &str) -> &'static str {
    match color_type {
        "mention" => backend::settings::DEFAULT_MENTION_COLOR,
        "whisper" => backend::settings::DEFAULT_WHISPER_COLOR,
        "subscription" => backend::settings::DEFAULT_SUBSCRIPTION_COLOR,
        "first_message" => backend::settings::DEFAULT_FIRST_MESSAGE_COLOR,
        "thread" => backend::settings::DEFAULT_THREAD_HIGHLIGHT_COLOR,
        "watchstreak" => backend::settings::DEFAULT_WATCHSTREAK_HIGHLIGHT_COLOR,
        "automod" => backend::settings::DEFAULT_AUTOMOD_HIGHLIGHT_COLOR,
        "announcement" => backend::settings::DEFAULT_ANNOUNCEMENT_HIGHLIGHT_COLOR,
        "raid" => backend::settings::DEFAULT_RAID_HIGHLIGHT_COLOR,
        "bits" => backend::settings::DEFAULT_BITS_HIGHLIGHT_COLOR,
        "phrase" => backend::settings::DEFAULT_PHRASE_HIGHLIGHT_COLOR,
        "redeem" => backend::settings::DEFAULT_REDEEM_HIGHLIGHT_COLOR,
        "elevated" => backend::settings::DEFAULT_ELEVATED_HIGHLIGHT_COLOR,
        "self_message" => backend::settings::DEFAULT_SELF_MESSAGE_HIGHLIGHT_COLOR,
        _ => backend::settings::DEFAULT_PHRASE_HIGHLIGHT_COLOR,
    }
}

fn highlight_default_alpha(color_type: &str) -> u8 {
    let alpha = match color_type {
        "mention" => colors::highlight_alpha::MENTION,
        "whisper" => colors::highlight_alpha::WHISPER,
        "subscription" => colors::highlight_alpha::SUBSCRIPTION,
        "first_message" => colors::highlight_alpha::FIRST_MESSAGE,
        "thread" => colors::highlight_alpha::REPLY_THREAD,
        "watchstreak" => colors::highlight_alpha::WATCH_STREAK,
        "automod" => colors::highlight_alpha::AUTOMOD,
        "announcement" => colors::highlight_alpha::ANNOUNCEMENT,
        "raid" | "bits" => colors::highlight_alpha::HIGHLIGHTED,
        "phrase" => colors::highlight_alpha::PHRASE,
        "redeem" => colors::highlight_alpha::REDEEMED,
        "elevated" => colors::highlight_alpha::ELEVATED,
        "self_message" => colors::highlight_alpha::SELF_MESSAGE,
        _ => colors::highlight_alpha::HIGHLIGHTED,
    };
    (alpha * 255.0).round() as u8
}

fn set_highlight_color(color_type: &str, hex: &str) {
    match color_type {
        "mention" => backend::set_mention_color(hex),
        "whisper" => backend::set_whisper_color(hex),
        "subscription" => backend::set_subscription_color(hex),
        "first_message" => backend::set_first_message_color(hex),
        "thread" => backend::set_thread_highlight_color(hex),
        "watchstreak" => crate::backend::settings::set_watchstreak_highlight_color(hex.to_string()),
        "automod" => crate::backend::settings::set_automod_highlight_color(hex.to_string()),
        "announcement" => {
            crate::backend::settings::set_announcement_highlight_color(hex.to_string())
        }
        "raid" => crate::backend::settings::set_raid_highlight_color(hex.to_string()),
        "bits" => crate::backend::settings::set_bits_highlight_color(hex.to_string()),
        "phrase" => crate::backend::settings::set_phrase_highlight_color(hex.to_string()),
        "redeem" => crate::backend::settings::set_redeem_highlight_color(hex.to_string()),
        "elevated" => crate::backend::settings::set_elevated_highlight_color(hex.to_string()),
        "self_message" => {
            crate::backend::settings::set_self_message_highlight_color(hex.to_string())
        }
        _ => {}
    }
}

fn render_highlight_blacklist_editor<T: 'static>(
    editor: HighlightBlacklistEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    pattern_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editing = editor.editing_id.is_some();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(if editing {
                    "Edit Ignored User"
                } else {
                    "Add Ignored User"
                }),
        )
        .child(setting_target(
            HIGHLIGHT_BLACKLIST,
            pattern_input.render(cx),
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .child(setting_target(
                    HIGHLIGHT_BLACKLIST_REGEX,
                    render_highlight_blacklist_toggle_chip(
                        "Regex",
                        editor.regex,
                        |cx| {
                            update_highlight_blacklist_editor(|editor| {
                                editor.regex = !editor.regex;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                ))
                .child(setting_target(
                    HIGHLIGHT_BLACKLIST_ENABLED,
                    render_highlight_blacklist_toggle_chip(
                        "Enabled",
                        editor.enabled,
                        |cx| {
                            update_highlight_blacklist_editor(|editor| {
                                editor.enabled = !editor.enabled;
                                editor.status = None;
                                editor.status_is_error = false;
                            });
                            cx.notify();
                        },
                        cx,
                    ),
                )),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(render_highlight_blacklist_button(
                    if editing { "Save" } else { "Add" },
                    true,
                    move |cx| {
                        save_highlight_blacklist_editor(editor_generation);
                        cx.notify();
                    },
                    cx,
                ))
                .when(editing, |row| {
                    row.child(render_highlight_blacklist_button(
                        "Cancel",
                        false,
                        |cx| {
                            reset_highlight_blacklist_editor();
                            cx.notify();
                        },
                        cx,
                    ))
                })
                .when_some(editor.status, |row, status| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(if editor.status_is_error || editor_needs_review {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        )
}

fn render_highlight_blacklist_list<T: 'static>(
    users: Vec<HighlightBlacklistUser>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(users.is_empty(), |el| {
            el.child(empty_state("No ignored users configured."))
        })
        .children(
            users
                .into_iter()
                .map(|user| render_highlight_blacklist_row(user, cx)),
        )
}

fn render_highlight_blacklist_row<T: 'static>(
    user: HighlightBlacklistUser,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let user_for_toggle = user.clone();
    let user_for_edit = user.clone();
    let user_id_for_delete = user.id.clone();
    let toggle_label = format!("Enable ignored-user rule {}", user.pattern);

    div()
        .w_full()
        .p_2()
        .bg(colors::bg_surface())
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_start()
        .gap_3()
        .child(labelled_toggle(
            SharedString::from(format!("highlight-blacklist-toggle-{user_id_for_delete}")),
            &toggle_label,
            user.enabled,
            cx,
            move |enabled, cx| {
                let mut updated = user_for_toggle.clone();
                updated.enabled = enabled;
                update_highlight_blacklist_user(&updated.id.clone(), updated);
                cx.notify();
            },
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_family("monospace")
                        .text_color(if user.enabled {
                            colors::text_primary()
                        } else {
                            colors::text_muted()
                        })
                        .child(user.pattern.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .items_center()
                        .child(render_highlight_blacklist_mode_chip(
                            if user.regex { "Regex" } else { "Literal" },
                            user.regex,
                        ))
                        .child(render_highlight_blacklist_mode_chip(
                            if user.enabled { "Enabled" } else { "Disabled" },
                            user.enabled,
                        )),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(render_highlight_blacklist_button(
                    "Edit",
                    false,
                    move |cx| {
                        set_highlight_blacklist_editor_for_user(user_for_edit.clone());
                        cx.notify();
                    },
                    cx,
                ))
                .child(action_button_keyed(
                    format!("delete-highlight-blacklist-{user_id_for_delete}"),
                    "Delete",
                    true,
                    cx,
                    move |cx| {
                        remove_highlight_blacklist_user(&user_id_for_delete);
                        if highlight_blacklist_editor_snapshot()
                            .draft
                            .editing_id
                            .as_deref()
                            == Some(user_id_for_delete.as_str())
                        {
                            reset_highlight_blacklist_editor();
                        }
                        cx.notify();
                    },
                )),
        )
}

fn highlight_blacklist_input<T: 'static>(
    key: &'static str,
    value: &str,
    placeholder: &'static str,
) -> DraftInput<T> {
    let field = HighlightBlacklistEditorField::Pattern;
    DraftInput::new(key, value, placeholder)
        .monospace(true)
        .on_focus(move |_cx| {
            update_highlight_blacklist_editor(|editor| editor.focused = Some(field));
            update_highlight_rule_editor(|editor| editor.focused = None);
        })
        .on_blur(move |_cx| {
            update_highlight_blacklist_editor(|editor| {
                if editor.focused == Some(field) {
                    editor.focused = None;
                }
            });
        })
        .on_change(|text, _cx| {
            update_highlight_blacklist_editor(|editor| {
                editor.pattern = text;
                editor.status = None;
                editor.status_is_error = false;
            });
        })
        .on_enter(|_cx| {
            if update_highlight_blacklist_editor(|editor| editor.focused = None)
                == EditorEditResult::Applied
            {
                save_highlight_blacklist_editor(highlight_blacklist_editor_snapshot().generation);
            }
        })
        .on_escape(|_window, _cx| {
            update_highlight_blacklist_editor(|editor| editor.focused = None);
        })
}

fn render_highlight_blacklist_toggle_chip<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &'static str,
    is_on: bool,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    render_highlight_toggle_chip(
        SharedString::from(format!(
            "highlight-blacklist-chip-{}",
            crate::components::settings::widgets::control_slug(label)
        )),
        label,
        is_on,
        on_click,
        cx,
    )
}

fn render_highlight_toggle_chip<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    id: SharedString,
    label: &'static str,
    is_on: bool,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let on_click = std::rc::Rc::new(on_click);
    let activate = on_click.clone();
    let entity = cx.entity();
    div()
        .id(id)
        .role(Role::Switch)
        .map(|control| crate::components::settings::widgets::settings_keyboard_control(control, cx))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(label))
        .aria_toggled(if is_on { Toggled::True } else { Toggled::False })
        .hover(|s| s.bg(colors::bg_hover()))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .rounded(Radii::md())
        .border_1()
        .border_color(if is_on {
            colors::accent()
        } else {
            colors::border()
        })
        .bg(if is_on {
            colors::accent().opacity(0.14)
        } else {
            colors::bg_surface()
        })
        .cursor_pointer()
        .child(render_highlight_blacklist_small_toggle(is_on))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if is_on {
                    colors::accent()
                } else {
                    colors::text_secondary()
                })
                .child(label),
        )
        .on_activate(cx.listener(move |_this, _event, _window, cx| activate(cx)))
        .on_a11y_action(AccessibleAction::Click, move |_, _, app| {
            entity.update(app, |_, cx| on_click(cx));
        })
}

fn render_highlight_blacklist_mode_chip(label: &'static str, active: bool) -> impl IntoElement {
    div()
        .px_1p5()
        .py(px(Spacing::XXS))
        .rounded(Radii::sm())
        .bg(if active {
            colors::accent().opacity(0.16)
        } else {
            colors::bg_elevated()
        })
        .text_xs()
        .text_color(if active {
            colors::accent()
        } else {
            colors::text_secondary()
        })
        .child(label)
}

/// Compile a rule pattern the way highlight matching does, so a pattern that
/// could never match is rejected when it is saved.
fn compile_highlight_regex(pattern: &str, case_sensitive: bool) -> Result<(), regex::Error> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .unicode(true)
        .build()
        .map(|_| ())
}

fn save_highlight_blacklist_editor(expected_generation: u64) {
    let Some(editor) = prepare_highlight_blacklist_editor(expected_generation) else {
        return;
    };
    let pattern = editor.pattern.trim().to_string();

    if pattern.is_empty() {
        update_highlight_blacklist_editor(|editor| {
            editor.status = Some("Pattern is required".to_string());
            editor.status_is_error = true;
        });
        return;
    }
    if editor.regex
        && let Err(error) = compile_highlight_regex(&pattern, false)
    {
        update_highlight_blacklist_editor(|editor| {
            editor.status = Some(format!(
                "Invalid regex: {}",
                super::brief_regex_error(&error)
            ));
            editor.status_is_error = true;
        });
        return;
    }

    let user = HighlightBlacklistUser {
        id: editor
            .editing_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        pattern,
        regex: editor.regex,
        enabled: editor.enabled,
    };

    let status = if let Some(editing_id) = editor.editing_id.as_deref() {
        if !get_highlight_blacklist_users()
            .iter()
            .any(|existing| existing.id == editing_id)
        {
            HIGHLIGHT_BLACKLIST_EDITOR.replace_from_authority(HighlightBlacklistEditorState {
                status: Some("Ignored user no longer exists; the draft was reset".to_string()),
                status_is_error: true,
                ..HighlightBlacklistEditorState::default()
            });
            return;
        }
        update_highlight_blacklist_user(&user.id.clone(), user);
        "Updated ignored user"
    } else {
        add_highlight_blacklist_user(user);
        "Added ignored user"
    };

    set_highlight_blacklist_editor_state(HighlightBlacklistEditorState {
        status: Some(status.to_string()),
        ..HighlightBlacklistEditorState::default()
    });
}

fn save_highlight_rule_editor(expected_generation: u64) {
    let Some(editor) = prepare_highlight_rule_editor(expected_generation) else {
        return;
    };
    let pattern = editor.pattern.trim().to_string();

    if pattern.is_empty() {
        update_highlight_rule_editor(|editor| {
            editor.status = Some("Pattern is required".to_string());
            editor.status_is_error = true;
        });
        return;
    }
    if editor.regex
        && let Err(error) = compile_highlight_regex(&pattern, editor.case_sensitive)
    {
        update_highlight_rule_editor(|editor| {
            editor.status = Some(format!(
                "Invalid regex: {}",
                super::brief_regex_error(&error)
            ));
            editor.status_is_error = true;
        });
        return;
    }

    let color = match normalize_highlight_rule_color(&editor.color) {
        Ok(color) => color,
        Err(message) => {
            update_highlight_rule_editor(|editor| {
                editor.status = Some(message.to_string());
                editor.status_is_error = true;
            });
            return;
        }
    };
    let sound_path = normalize_highlight_rule_sound_path(&editor.sound_path);

    let rule = HighlightRule {
        id: editor
            .editing_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        target: editor.target,
        pattern,
        regex: editor.regex,
        case_sensitive: editor.case_sensitive,
        show_in_mentions: editor.show_in_mentions,
        play_sound: editor.play_sound,
        alert: editor.alert,
        sound_path,
        enabled: editor.enabled,
        color,
    };

    let status = if let Some(editing_id) = editor.editing_id.as_deref() {
        if !get_highlight_rules()
            .iter()
            .any(|existing| existing.id == editing_id)
        {
            HIGHLIGHT_RULE_EDITOR.replace_from_authority(HighlightRuleEditorState {
                status: Some("Highlight rule no longer exists; the draft was reset".to_string()),
                status_is_error: true,
                ..HighlightRuleEditorState::default()
            });
            return;
        }
        update_highlight_rule(&rule.id.clone(), rule);
        "Updated highlight rule"
    } else {
        add_highlight_rule(rule);
        "Added highlight rule"
    };

    set_highlight_rule_editor_state(HighlightRuleEditorState {
        status: Some(status.to_string()),
        ..HighlightRuleEditorState::default()
    });
}

fn set_highlight_rule_editor_for_rule(rule: HighlightRule) {
    let mut editor = highlight_rule_editor_from_rule(&rule);
    editor.focused = Some(HighlightRuleEditorField::Pattern);
    HIGHLIGHT_RULE_EDITOR.replace_from_authority(editor);
    draft_input::request_focus(CUSTOM_HIGHLIGHT_RULES.control_id);
    update_highlight_blacklist_editor(|editor| editor.focused = None);
}

/// Where "Edit rule" from chat lands, and why it fell back to the rule list.
pub(crate) struct HighlightRuleEditorOpen {
    pub(crate) request: SettingsOpenRequest,
    /// Title and detail for a warning toast when the rule was not loaded.
    pub(crate) problem: Option<(&'static str, &'static str)>,
}

/// Open the rule editor on `rule`, loading it exactly as its Edit button does.
/// A missing rule, or unsaved changes to a different draft, open the rule list
/// instead so nothing typed is lost.
pub(crate) fn highlight_rule_editor_request(
    rule: Option<HighlightRule>,
) -> HighlightRuleEditorOpen {
    let request = SettingsOpenRequest::at(SettingsLocation::anchor(
        SettingsSectionId::Highlights,
        SettingsSubsectionId::HighlightsCustomRules,
    ));
    let Some(rule) = rule else {
        return HighlightRuleEditorOpen {
            request,
            problem: Some((
                "Couldn't find that highlight rule",
                "It was edited or removed after this message arrived. Your highlight rules are open instead.",
            )),
        };
    };
    let editor = highlight_rule_editor_snapshot().draft;
    if editor.editing_id.as_deref() == Some(rule.id.as_str()) {
        // Already loaded: keep whatever is being typed into it.
        draft_input::request_focus(CUSTOM_HIGHLIGHT_RULES.control_id);
    } else if highlight_rule_draft_has_unsaved_changes(&editor) {
        return HighlightRuleEditorOpen {
            request,
            problem: Some((
                "Finish or discard the rule you're editing first",
                "Your unsaved rule is still in the editor. Save or cancel it, then choose Edit rule again.",
            )),
        };
    } else {
        set_highlight_rule_editor_for_rule(rule);
    }
    HighlightRuleEditorOpen {
        request: request.with_setting(CUSTOM_HIGHLIGHT_RULES.id),
        problem: None,
    }
}

/// The editor differs from the rule it edits, or from an empty new rule.
/// A draft for a rule that no longer exists cannot be saved, so it does not
/// count.
fn highlight_rule_draft_has_unsaved_changes(editor: &HighlightRuleEditorState) -> bool {
    let baseline = match editor.editing_id.as_deref() {
        Some(id) => match get_highlight_rules().iter().find(|rule| rule.id == id) {
            Some(rule) => highlight_rule_editor_from_rule(rule),
            None => return false,
        },
        None => HighlightRuleEditorState::default(),
    };
    editor.target != baseline.target
        || editor.pattern != baseline.pattern
        || editor.regex != baseline.regex
        || editor.case_sensitive != baseline.case_sensitive
        || editor.show_in_mentions != baseline.show_in_mentions
        || editor.play_sound != baseline.play_sound
        || editor.alert != baseline.alert
        || editor.enabled != baseline.enabled
        || editor.color != baseline.color
        || editor.sound_path != baseline.sound_path
}

fn reset_highlight_rule_editor() {
    set_highlight_rule_editor_state(HighlightRuleEditorState::default());
}

pub(super) fn reset_highlight_editors_after_settings_replace() {
    set_highlight_rule_editor_state(HighlightRuleEditorState {
        status: Some("Highlight-rule draft reset after settings were replaced".to_string()),
        ..HighlightRuleEditorState::default()
    });
    set_highlight_blacklist_editor_state(HighlightBlacklistEditorState {
        status: Some("Ignored-user draft reset after settings were replaced".to_string()),
        ..HighlightBlacklistEditorState::default()
    });
}

fn set_highlight_blacklist_editor_for_user(user: HighlightBlacklistUser) {
    let mut editor = highlight_blacklist_editor_from_user(&user);
    editor.focused = Some(HighlightBlacklistEditorField::Pattern);
    HIGHLIGHT_BLACKLIST_EDITOR.replace_from_authority(editor);
    draft_input::request_focus(HIGHLIGHT_BLACKLIST.control_id);
    update_highlight_rule_editor(|editor| editor.focused = None);
}

fn reset_highlight_blacklist_editor() {
    set_highlight_blacklist_editor_state(HighlightBlacklistEditorState::default());
}

fn highlight_blacklist_editor_from_user(
    user: &HighlightBlacklistUser,
) -> HighlightBlacklistEditorState {
    HighlightBlacklistEditorState {
        pattern: user.pattern.clone(),
        editing_id: Some(user.id.clone()),
        regex: user.regex,
        enabled: user.enabled,
        status: None,
        status_is_error: false,
        focused: None,
    }
}

fn recovered_highlight_blacklist_editor_from(
    previous: &HighlightBlacklistEditorState,
    users: &[HighlightBlacklistUser],
) -> HighlightBlacklistEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| users.iter().find(|user| user.id == editing_id))
        .map(highlight_blacklist_editor_from_user)
        .unwrap_or_default();
    editor.status = Some(
        "Ignored-user draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor.status_is_error = true;
    editor
}

fn recovered_highlight_blacklist_editor(
    previous: &HighlightBlacklistEditorState,
) -> HighlightBlacklistEditorState {
    let users = get_highlight_blacklist_users();
    recovered_highlight_blacklist_editor_from(previous, &users)
}

fn highlight_blacklist_editor_snapshot_from(
    users: &[HighlightBlacklistUser],
) -> EditorSnapshot<HighlightBlacklistEditorState> {
    HIGHLIGHT_BLACKLIST_EDITOR
        .snapshot_or_reset(|previous| recovered_highlight_blacklist_editor_from(previous, users))
}

fn highlight_blacklist_editor_snapshot() -> EditorSnapshot<HighlightBlacklistEditorState> {
    HIGHLIGHT_BLACKLIST_EDITOR.snapshot_or_reset(recovered_highlight_blacklist_editor)
}

fn prepare_highlight_blacklist_editor(
    expected_generation: u64,
) -> Option<HighlightBlacklistEditorState> {
    HIGHLIGHT_BLACKLIST_EDITOR
        .prepare_ready(expected_generation, recovered_highlight_blacklist_editor)
}

fn highlight_rule_editor_from_rule(rule: &HighlightRule) -> HighlightRuleEditorState {
    HighlightRuleEditorState {
        target: rule.target,
        pattern: rule.pattern.clone(),
        regex: rule.regex,
        case_sensitive: rule.case_sensitive,
        show_in_mentions: rule.show_in_mentions,
        play_sound: rule.play_sound,
        alert: rule.alert,
        enabled: rule.enabled,
        color: rule.color.clone().unwrap_or_default(),
        sound_path: rule.sound_path.clone().unwrap_or_default(),
        editing_id: Some(rule.id.clone()),
        status: None,
        status_is_error: false,
        focused: None,
    }
}

fn recovered_highlight_rule_editor_from(
    previous: &HighlightRuleEditorState,
    rules: &[HighlightRule],
) -> HighlightRuleEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| rules.iter().find(|rule| rule.id == editing_id))
        .map(highlight_rule_editor_from_rule)
        .unwrap_or_default();
    editor.status = Some(
        "Highlight-rule draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor.status_is_error = true;
    editor
}

fn recovered_highlight_rule_editor(
    previous: &HighlightRuleEditorState,
) -> HighlightRuleEditorState {
    let rules = get_highlight_rules();
    recovered_highlight_rule_editor_from(previous, &rules)
}

fn highlight_rule_editor_snapshot_from(
    rules: &[HighlightRule],
) -> EditorSnapshot<HighlightRuleEditorState> {
    HIGHLIGHT_RULE_EDITOR
        .snapshot_or_reset(|previous| recovered_highlight_rule_editor_from(previous, rules))
}

fn highlight_rule_editor_snapshot() -> EditorSnapshot<HighlightRuleEditorState> {
    HIGHLIGHT_RULE_EDITOR.snapshot_or_reset(recovered_highlight_rule_editor)
}

fn prepare_highlight_rule_editor(expected_generation: u64) -> Option<HighlightRuleEditorState> {
    HIGHLIGHT_RULE_EDITOR.prepare_ready(expected_generation, recovered_highlight_rule_editor)
}

fn set_highlight_blacklist_editor_state(state: HighlightBlacklistEditorState) {
    HIGHLIGHT_BLACKLIST_EDITOR.replace_from_authority(state);
}

fn set_highlight_rule_editor_state(state: HighlightRuleEditorState) {
    HIGHLIGHT_RULE_EDITOR.replace_from_authority(state);
}

fn update_highlight_blacklist_editor(
    update: impl FnOnce(&mut HighlightBlacklistEditorState),
) -> EditorEditResult {
    HIGHLIGHT_BLACKLIST_EDITOR.apply_user_edit(recovered_highlight_blacklist_editor, update)
}

fn update_highlight_rule_editor(
    update: impl FnOnce(&mut HighlightRuleEditorState),
) -> EditorEditResult {
    HIGHLIGHT_RULE_EDITOR.apply_user_edit(recovered_highlight_rule_editor, update)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn rule(id: &str) -> HighlightRule {
        HighlightRule {
            id: id.to_string(),
            target: HighlightRuleTarget::Badge,
            pattern: "subscriber".to_string(),
            regex: true,
            case_sensitive: true,
            show_in_mentions: false,
            play_sound: true,
            alert: true,
            sound_path: Some("sound.wav".to_string()),
            enabled: false,
            color: Some("#80ff0000".to_string()),
        }
    }

    fn blacklist_user(id: &str) -> HighlightBlacklistUser {
        HighlightBlacklistUser {
            id: id.to_string(),
            pattern: "ignored.*".to_string(),
            regex: true,
            enabled: false,
        }
    }

    #[test]
    fn rule_summary_prioritizes_needs_review_over_an_empty_draft() {
        let rule_editor = EditorSnapshot {
            draft: HighlightRuleEditorState::default(),
            generation: 0,
            needs_review: true,
        };
        let blacklist_editor = EditorSnapshot {
            draft: HighlightBlacklistEditorState::default(),
            generation: 0,
            needs_review: false,
        };

        assert_eq!(
            highlight_rules_summary(0, &rule_editor, 0, &blacklist_editor),
            "0 rules · 0 exclusions · Needs review"
        );
    }

    #[test]
    fn rule_recovery_restores_every_persisted_field_by_id() {
        let previous = HighlightRuleEditorState {
            pattern: "partial".to_string(),
            sound_path: "partial".to_string(),
            editing_id: Some("two".to_string()),
            focused: Some(HighlightRuleEditorField::SoundPath),
            ..HighlightRuleEditorState::default()
        };
        let rules = [rule("one"), rule("two")];

        let recovered = recovered_highlight_rule_editor_from(&previous, &rules);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.target, HighlightRuleTarget::Badge);
        assert_eq!(recovered.pattern, "subscriber");
        assert!(recovered.regex);
        assert!(recovered.case_sensitive);
        assert!(!recovered.show_in_mentions);
        assert!(recovered.play_sound);
        assert!(recovered.alert);
        assert!(!recovered.enabled);
        assert_eq!(recovered.color, "#80ff0000");
        assert_eq!(recovered.sound_path, "sound.wav");
        assert!(recovered.focused.is_none());
        assert!(recovered.status_is_error);
    }

    #[test]
    fn removed_rule_recovers_as_a_marked_add_form() {
        let previous = HighlightRuleEditorState {
            editing_id: Some("removed".to_string()),
            pattern: "partial".to_string(),
            ..HighlightRuleEditorState::default()
        };

        let recovered = recovered_highlight_rule_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.pattern.is_empty());
        assert_eq!(recovered.target, HighlightRuleTarget::Phrase);
        assert!(recovered.status_is_error);
    }

    #[test]
    fn blacklist_recovery_restores_every_persisted_field_by_id() {
        let previous = HighlightBlacklistEditorState {
            pattern: "partial".to_string(),
            editing_id: Some("two".to_string()),
            focused: Some(HighlightBlacklistEditorField::Pattern),
            ..HighlightBlacklistEditorState::default()
        };
        let users = [blacklist_user("one"), blacklist_user("two")];

        let recovered = recovered_highlight_blacklist_editor_from(&previous, &users);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.pattern, "ignored.*");
        assert!(recovered.regex);
        assert!(!recovered.enabled);
        assert!(recovered.focused.is_none());
        assert!(recovered.status_is_error);
    }

    #[test]
    fn removed_blacklist_user_recovers_as_a_marked_add_form() {
        let previous = HighlightBlacklistEditorState {
            editing_id: Some("removed".to_string()),
            pattern: "partial".to_string(),
            ..HighlightBlacklistEditorState::default()
        };

        let recovered = recovered_highlight_blacklist_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.pattern.is_empty());
        assert!(recovered.status_is_error);
    }
}
