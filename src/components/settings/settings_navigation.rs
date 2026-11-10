//! Settings navigation hierarchy.
//!
//! Single source of truth for the three-level settings tree:
//!
//! ```text
//! Category
//! └── Page (SettingsSectionId)
//!     └── Subsection scroll anchor (SettingsSubsectionId)
//! ```
//!
//! Individual settings are flat search results; browsing retains the tree.
//! [`SettingDescriptor`] constants are shared by the renderers and search,
//! so match counts stay trustworthy without a hand-maintained keyword copy.
//!
//! Everything here is compile-time data. Search, sidebar/tree rendering, label
//! lookups, and slug normalization all borrow from the same static tables
//! instead of rebuilding the hierarchy on every render.
//!
//! Persistence lives in [`crate::backend::settings`] as
//! [`SettingsNavigationPrefs`] (slug-keyed, additive, serde-defaulted); this
//! module owns the typed <-> slug conversions and the fallback rules.

use super::sections::{
    ACCOUNT_SETTINGS, APPEARANCE_CHAT_DISPLAY_SETTINGS, APPEARANCE_MOTION_SETTINGS,
    APPEARANCE_OPTIONS_SETTINGS, APPEARANCE_TABS_PRIMARY_SETTINGS, APPEARANCE_TABS_SIZING_SETTINGS,
    APPEARANCE_THEME_COLORS_SETTINGS, APPEARANCE_THEME_PRIMARY_SETTINGS,
    APPEARANCE_WINDOWS_OVERLAY_SETTINGS, CHAT_COMPOSING_ALL_SETTINGS, CHAT_EMOTES_ALL_SETTINGS,
    CHAT_INPUT_COMPOSING_SETTINGS, CHAT_MESSAGE_BEHAVIOR_SETTINGS, CHAT_OPTIONS_SETTINGS,
    CHAT_READING_ALL_SETTINGS, CHAT_TIMESTAMPS_SETTINGS, COMMANDS_CUSTOM_SETTINGS,
    COMMANDS_SLASH_SETTINGS, DEVELOPER_CHAT_LOGGING_SETTINGS, DEVELOPER_CONFIGURATION_SETTINGS,
    DEVELOPER_DIAGNOSTICS_SETTINGS, DEVELOPER_HISTORY_CLEANUP_SETTINGS,
    DEVELOPER_NETWORK_PROXY_SETTINGS, EMOTES_COSMETICS_SETTINGS, EMOTES_DISPLAY_SETTINGS,
    EMOTES_PROVIDERS_SETTINGS, FILTERS_BLOCKED_USERS_SETTINGS, FILTERS_RULES_SETTINGS,
    FILTERS_SIMILAR_MESSAGES_SETTINGS, GENERAL_SETTINGS_WINDOW_SETTINGS,
    HIGHLIGHTS_BEHAVIOR_SETTINGS, HIGHLIGHTS_CUSTOM_RULES_SETTINGS,
    HIGHLIGHTS_PREVIEW_COLORS_SETTINGS, HOTKEYS_CHAT_SETTINGS, HOTKEYS_CUSTOM_SETTINGS,
    HOTKEYS_MODERATION_SETTINGS, HOTKEYS_NAVIGATION_SETTINGS, HOTKEYS_OTHER_SETTINGS,
    HOTKEYS_SEARCH_SETTINGS, LINKS_MEDIA_IMAGE_UPLOADS_SETTINGS,
    LINKS_MEDIA_LINK_PREVIEWS_SETTINGS, LINKS_MEDIA_LINKS_SETTINGS, LINKS_MEDIA_PLAYERS_SETTINGS,
    MODERATION_BUTTONS_ALL_SETTINGS, MODERATION_BUTTONS_SETTINGS,
    MODERATION_DELETED_MESSAGES_SETTINGS, MODERATION_USER_CARD_SETTINGS,
    MODERATION_VARIABLES_SETTINGS, NOTIFICATIONS_DESKTOP_SETTINGS, NOTIFICATIONS_LIVE_SETTINGS,
    NOTIFICATIONS_MUTED_SETTINGS, NOTIFICATIONS_PRIMARY_SETTINGS, NOTIFICATIONS_SOUNDS_SETTINGS,
    SHORTCUTS_ALL_SETTINGS, STREAMER_MODE_DATA_SETTINGS, STREAMER_MODE_GENERAL_SETTINGS,
    STREAMER_MODE_NOTIFICATIONS_SETTINGS, STREAMER_MODE_PRIVACY_SETTINGS, UPDATES_CHECKS_SETTINGS,
    UPDATES_GENERAL_SETTINGS, UPDATES_RELEASE_SETTINGS,
};
use crate::backend::settings::{PersistedSettingsLocation, SettingsNavigationPrefs};

/// Page identifiers.
///
/// NOTE: variants are stable IDs (state stores the selected page by ID, not
/// index). Several older standalone pages (Embeds, Uploader, Proxy, Logging,
/// Debug, Similarity) were merged into Links & Media / Developer Tools /
/// Filters and no longer have their own destination; their old slugs survive
/// in [`LEGACY_SLUGS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SettingsSectionId {
    #[default]
    General,
    Theme,
    Sounds,
    Updates,
    ChatBehavior,
    Emotes,
    Highlights,
    Filters,
    Nicknames,
    Commands,
    Moderation,
    Hotkeys,
    Connections,
    Privacy,
    Advanced,
}

/// Top-level tree categories. Categories are expansion containers, never
/// content destinations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsCategoryId {
    General,
    Chat,
    Tools,
    MediaPrivacy,
    Developer,
}

/// Stable subsection (scroll anchor) identifiers.
///
/// One variant per anchor in the information architecture. Variant names are
/// page-qualified where a slug repeats across pages (`chat`, `moderation`,
/// `notifications`, ...); slugs stay short because they are only unique within
/// their page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsSubsectionId {
    Account,
    SettingsWindow,
    ThemeColors,
    AppearanceMotion,
    ChatDisplay,
    TabsSizing,
    /// The physical anchor for the collapsed secondary appearance controls.
    /// `WindowsOverlay` remains a legacy identifier and resolves here.
    AppearanceOptions,
    WindowsOverlay,
    Sounds,
    DesktopNotifications,
    LiveChannels,
    MutedChannels,
    UpdateChecks,
    CurrentRelease,
    InputComposing,
    MessageBehavior,
    Timestamps,
    ChatReading,
    ChatComposing,
    ChatOptions,
    EmotesDisplay,
    EmotesProviders,
    EmotesCosmetics,
    NicknameRules,
    HighlightsPreviewColors,
    HighlightsBehavior,
    HighlightsCustomRules,
    FilterRules,
    BlockedUsers,
    SimilarMessages,
    SlashCommands,
    CustomCommands,
    UserCard,
    ModerationButtons,
    ModerationVariables,
    DeletedMessages,
    HotkeysNavigation,
    HotkeysChat,
    HotkeysSearch,
    HotkeysModeration,
    HotkeysOther,
    HotkeysCustom,
    ImageUploads,
    LinkPreviews,
    Links,
    Players,
    StreamerPrivacy,
    StreamerNotifications,
    DataManagement,
    NetworkProxy,
    ChatLogging,
    HistoryCleanup,
    Diagnostics,
    DeveloperMode,
    Configuration,
}

/// Disclosure rule for a category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SettingsVisibility {
    /// Always part of the tree.
    #[default]
    Always,
    /// Only present while developer mode (or the benchmark override) is on.
    DeveloperOnly,
}

impl SettingsVisibility {
    pub const fn is_visible(self, developer_visible: bool) -> bool {
        match self {
            Self::Always => true,
            Self::DeveloperOnly => developer_visible,
        }
    }

    pub const fn is_developer_only(self) -> bool {
        matches!(self, Self::DeveloperOnly)
    }
}

/// A resolved content destination: a page plus an optional scroll anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettingsLocation {
    pub section: SettingsSectionId,
    pub subsection: Option<SettingsSubsectionId>,
}

impl Default for SettingsLocation {
    fn default() -> Self {
        Self::page(SettingsSectionId::General)
    }
}

impl SettingsLocation {
    pub const fn page(section: SettingsSectionId) -> Self {
        Self {
            section,
            subsection: None,
        }
    }

    pub const fn anchor(section: SettingsSectionId, subsection: SettingsSubsectionId) -> Self {
        Self {
            section,
            subsection: Some(subsection),
        }
    }

    /// This page's default anchor (its first subsection), if it has one.
    pub fn with_default_subsection(self) -> Self {
        let self_ = canonical_location(self);
        match section_descriptor(self_.section) {
            Some(section) if self_.subsection.is_none() => Self {
                section: self_.section,
                subsection: section.default_subsection,
            },
            _ => self_,
        }
    }

    pub fn is_developer_only(self) -> bool {
        self.subsection.is_some_and(subsection_is_developer_only)
    }

    pub fn is_visible(self, developer_visible: bool) -> bool {
        let location = canonical_location(self);
        section_visibility(location.section).is_visible(developer_visible)
            && (developer_visible || !location.is_developer_only())
    }

    /// Slug pair written to disk. Locations are persisted by slug so renaming a
    /// Rust variant never invalidates a user's stored position.
    pub fn to_persisted(self) -> PersistedSettingsLocation {
        let self_ = canonical_location(self);
        PersistedSettingsLocation {
            section: section_slug(self_.section).to_string(),
            subsection: self_
                .subsection
                .and_then(|id| subsection_descriptor(self_.section, id))
                .map(|descriptor| descriptor.slug.to_string()),
        }
    }

    /// Inverse of [`Self::to_persisted`] with the spec's fallback rules applied:
    /// an unknown subsection degrades to the page default, an unknown page
    /// yields `None` so the caller can fall back further.
    pub fn from_persisted(
        persisted: &PersistedSettingsLocation,
        developer_visible: bool,
    ) -> Option<Self> {
        resolve_slug_path(
            &persisted.section,
            persisted.subsection.as_deref(),
            developer_visible,
        )
    }
}

/// Stable destinations used by the flat settings sidebar. Older section IDs
/// remain part of the public enum because locations are persisted and callers
/// still use them, but always resolve to one of these eight pages.
pub const fn canonical_section(id: SettingsSectionId) -> SettingsSectionId {
    match id {
        SettingsSectionId::General
        | SettingsSectionId::Theme
        | SettingsSectionId::ChatBehavior
        | SettingsSectionId::Highlights
        | SettingsSectionId::Moderation
        | SettingsSectionId::Connections
        | SettingsSectionId::Hotkeys
        | SettingsSectionId::Advanced => id,
        SettingsSectionId::Sounds => SettingsSectionId::Highlights,
        SettingsSectionId::Updates | SettingsSectionId::Privacy => SettingsSectionId::General,
        SettingsSectionId::Emotes | SettingsSectionId::Nicknames => SettingsSectionId::ChatBehavior,
        SettingsSectionId::Filters => SettingsSectionId::Moderation,
        SettingsSectionId::Commands => SettingsSectionId::Hotkeys,
    }
}

/// Canonicalize legacy page IDs and resolve every anchor by its global owner.
/// This makes persisted page/anchor pairs and explicit open requests survive
/// the flat-page migration even when the old pair no longer belongs together.
pub fn canonical_location(location: SettingsLocation) -> SettingsLocation {
    let subsection = location
        .subsection
        .or_else(|| legacy_default_subsection(location.section))
        .map(canonical_subsection);
    let section = subsection
        .and_then(subsection_owner)
        .unwrap_or_else(|| canonical_section(location.section));
    SettingsLocation {
        section,
        subsection,
    }
}

/// Resolve retained legacy anchors to the visible group that now owns their
/// controls. This happens before ownership lookup so persisted old pairs,
/// direct callers, and search targets all agree on one physical block.
const fn canonical_subsection(id: SettingsSubsectionId) -> SettingsSubsectionId {
    match id {
        SettingsSubsectionId::StreamerNotifications => SettingsSubsectionId::StreamerPrivacy,
        SettingsSubsectionId::CurrentRelease => SettingsSubsectionId::UpdateChecks,
        SettingsSubsectionId::LiveChannels => SettingsSubsectionId::DesktopNotifications,
        SettingsSubsectionId::ModerationVariables => SettingsSubsectionId::ModerationButtons,
        SettingsSubsectionId::WindowsOverlay => SettingsSubsectionId::AppearanceOptions,
        SettingsSubsectionId::InputComposing => SettingsSubsectionId::ChatComposing,
        SettingsSubsectionId::MessageBehavior
        | SettingsSubsectionId::Timestamps
        | SettingsSubsectionId::ChatDisplay => SettingsSubsectionId::ChatReading,
        SettingsSubsectionId::EmotesProviders | SettingsSubsectionId::EmotesCosmetics => {
            SettingsSubsectionId::EmotesDisplay
        }
        SettingsSubsectionId::NicknameRules => SettingsSubsectionId::ChatOptions,
        SettingsSubsectionId::HotkeysChat
        | SettingsSubsectionId::HotkeysSearch
        | SettingsSubsectionId::HotkeysModeration
        | SettingsSubsectionId::HotkeysOther
        | SettingsSubsectionId::HotkeysCustom => SettingsSubsectionId::HotkeysNavigation,
        _ => id,
    }
}

const fn legacy_default_subsection(section: SettingsSectionId) -> Option<SettingsSubsectionId> {
    match section {
        SettingsSectionId::Sounds => Some(SettingsSubsectionId::Sounds),
        SettingsSectionId::Updates => Some(SettingsSubsectionId::UpdateChecks),
        SettingsSectionId::Emotes => Some(SettingsSubsectionId::EmotesDisplay),
        SettingsSectionId::Filters => Some(SettingsSubsectionId::FilterRules),
        SettingsSectionId::Nicknames => Some(SettingsSubsectionId::NicknameRules),
        SettingsSectionId::Commands => Some(SettingsSubsectionId::SlashCommands),
        SettingsSectionId::Privacy => Some(SettingsSubsectionId::StreamerPrivacy),
        _ => None,
    }
}

/// The original anchor requested by a caller before the flat page migration
/// maps it to a visible block. Its disclosure chain is needed for legacy page
/// requests such as `Nicknames`, whose default is now a summary row.
pub fn requested_anchor(location: SettingsLocation) -> Option<SettingsLocation> {
    match location.subsection {
        Some(_) => Some(location),
        None => legacy_default_subsection(location.section)
            .map(|subsection| SettingsLocation::anchor(location.section, subsection)),
    }
}

/// One user-visible setting. Sections declare these as constants and hand the
/// same value to both the widget (label + control id) and search (aliases), so
/// a rendered row and a search hit can never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
    /// Primitive/legacy identity; exact row addressing uses `id`.
    pub control_id: &'static str,
    pub disclosures: &'static [&'static str],
}

/// A stable row destination; disclosure IDs describe presentation, not settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingTarget {
    pub location: SettingsLocation,
    pub setting: SettingDescriptor,
}

impl SettingTarget {
    pub fn row_id(self) -> &'static str {
        self.setting.id
    }

    pub fn is_developer_only(self) -> bool {
        self.location.is_developer_only()
    }
}

pub fn setting_targets() -> impl Iterator<Item = SettingTarget> {
    SECTIONS.iter().flat_map(|section| {
        section
            .settings
            .iter()
            .map(move |setting| SettingTarget {
                location: SettingsLocation::page(section.id),
                setting: *setting,
            })
            .chain(section.subsections.iter().flat_map(move |subsection| {
                subsection
                    .settings
                    .iter()
                    .map(move |setting| SettingTarget {
                        location: SettingsLocation::anchor(section.id, subsection.id),
                        setting: *setting,
                    })
            }))
    })
}

pub fn setting_target_by_id(id: &str) -> Option<SettingTarget> {
    setting_targets().find(|target| target.setting.id == id)
}

/// One scroll anchor inside a page.
#[derive(Clone, Copy, Debug)]
pub struct SettingsSubsectionDescriptor {
    pub id: SettingsSubsectionId,
    pub slug: &'static str,
    pub label: &'static str,
    pub keywords: &'static [&'static str],
    pub settings: &'static [SettingDescriptor],
}

/// One page (a selectable content destination).
#[derive(Clone, Copy, Debug)]
pub struct SettingsSectionDescriptor {
    pub id: SettingsSectionId,
    pub slug: &'static str,
    /// Slugs from earlier layouts that still resolve here. Subsection-specific
    /// legacy slugs additionally appear in [`LEGACY_SLUGS`].
    pub legacy_slugs: &'static [&'static str],
    pub label: &'static str,
    /// Compact navigation icon rendered beside the destination label.
    pub icon: &'static str,
    /// Keywords for search matching.
    pub keywords: &'static [&'static str],
    pub default_subsection: Option<SettingsSubsectionId>,
    pub subsections: &'static [SettingsSubsectionDescriptor],
    /// Settings that belong to the page itself rather than to an anchor.
    ///
    /// Only leaf pages populate this: they render one unlabelled block and have
    /// no subsection to hang descriptors on, so without it their settings would
    /// be invisible to search and their match counts would read zero. Pages with
    /// anchors leave it empty and declare everything under
    /// [`SettingsSubsectionDescriptor::settings`], so nothing is counted twice.
    pub settings: &'static [SettingDescriptor],
}

impl SettingsSectionDescriptor {
    pub fn subsection(
        &self,
        id: SettingsSubsectionId,
    ) -> Option<&'static SettingsSubsectionDescriptor> {
        self.subsections.iter().find(|sub| sub.id == id)
    }

    pub fn subsection_by_slug(&self, slug: &str) -> Option<&'static SettingsSubsectionDescriptor> {
        self.subsections
            .iter()
            .find(|sub| sub.slug.eq_ignore_ascii_case(slug))
    }

    pub fn default_location(&self) -> SettingsLocation {
        SettingsLocation {
            section: self.id,
            subsection: self.default_subsection,
        }
    }
}

/// One tree category. Categories carry no content of their own.
#[derive(Clone, Copy, Debug)]
pub struct SettingsCategoryDescriptor {
    pub id: SettingsCategoryId,
    pub slug: &'static str,
    pub label: &'static str,
    pub sections: &'static [SettingsSectionId],
    pub visibility: SettingsVisibility,
}

impl SettingsCategoryDescriptor {
    /// Page descriptors in declaration order.
    pub fn section_descriptors(
        &self,
    ) -> impl Iterator<Item = &'static SettingsSectionDescriptor> + '_ {
        self.sections
            .iter()
            .filter_map(|id| section_descriptor(*id))
    }
}

/// Legacy slug -> location mapping for slugs that target a specific anchor.
#[derive(Clone, Copy, Debug)]
pub struct LegacySlugMapping {
    pub slug: &'static str,
    pub section: SettingsSectionId,
    pub subsection: Option<SettingsSubsectionId>,
}

/// Back-compat aliases so existing call sites keep compiling while the tree
/// shell is built out. `SettingsSection` was the flat pre-tree descriptor and
/// has the same `id`/`label`/`icon`/`keywords` shape.
pub type SettingsSection = SettingsSectionDescriptor;

const APPEARANCE_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ThemeColors,
        slug: "theme-colors",
        label: "Theme & colors",
        keywords: &[
            "live preview",
            "theme",
            "accent color",
            "dark",
            "light",
            "midnight",
            "oled",
            "custom theme",
            "theme file",
            "palette",
        ],
        settings: APPEARANCE_THEME_COLORS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatDisplay,
        slug: "chat-display",
        label: "Chat display",
        keywords: &[
            "chat names",
            "username",
            "display name",
            "localized",
            "gray names",
            "colorize users",
            "badge display",
            "badges",
            "animations",
            "animated emotes",
        ],
        settings: APPEARANCE_CHAT_DISPLAY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::TabsSizing,
        slug: "tabs-sizing",
        label: "Tabs & sizing",
        keywords: &[
            "tab style",
            "tab overflow",
            "unread indicator",
            "sizing",
            "font scale",
            "text size",
            "emote size",
            "image quality",
            "density",
        ],
        settings: APPEARANCE_TABS_SIZING_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::WindowsOverlay,
        slug: "windows-overlay",
        label: "Windows & overlay",
        keywords: &[
            "settings window",
            "popout",
            "window",
            "always on top",
            "transparent",
            "overlay",
            "click through",
            "ghost mode",
            "opacity",
            "idle fade",
        ],
        settings: APPEARANCE_WINDOWS_OVERLAY_SETTINGS,
    },
];

const NOTIFICATIONS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Sounds,
        slug: "sounds",
        label: "Sounds",
        keywords: &[
            "sound events",
            "audio",
            "volume",
            "highlight sound",
            "ping",
            "custom sound",
        ],
        settings: NOTIFICATIONS_SOUNDS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DesktopNotifications,
        slug: "desktop-notifications",
        label: "Desktop notifications",
        keywords: &["desktop notification", "toast", "system notification"],
        settings: NOTIFICATIONS_DESKTOP_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::LiveChannels,
        slug: "live-channels",
        label: "Live channels",
        keywords: &[
            "live channel",
            "online",
            "offline",
            "stream up",
            "startup delay",
        ],
        settings: NOTIFICATIONS_LIVE_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::MutedChannels,
        slug: "muted-channels",
        label: "Muted channels",
        keywords: &["mute", "muted channel", "silence", "highlight sound"],
        settings: NOTIFICATIONS_MUTED_SETTINGS,
    },
];

const UPDATES_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::UpdateChecks,
        slug: "update-checks",
        label: "Update checks",
        keywords: &[
            "update",
            "auto update",
            "check for updates",
            "download",
            "install",
            "restart",
            "signature",
            "update origin",
        ],
        settings: UPDATES_CHECKS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::CurrentRelease,
        slug: "current-release",
        label: "Current release",
        keywords: &["version", "release", "changelog", "build"],
        settings: UPDATES_RELEASE_SETTINGS,
    },
];

const CHAT_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::InputComposing,
        slug: "input-composing",
        label: "Input & composing",
        keywords: &[
            "input",
            "send button",
            "character counter",
            "tab completion",
            "username completion",
            "mention comma",
            "unified input",
            "spell check",
        ],
        settings: CHAT_INPUT_COMPOSING_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::MessageBehavior,
        slug: "message-behavior",
        label: "Message behavior",
        keywords: &[
            "ascii",
            "ascii art",
            "collapse",
            "reply button",
            "username right click",
            "modifier",
            "alternate click",
            "hover pause",
            "pause indicator",
            "split header",
            "one line header",
            "compact header",
        ],
        settings: CHAT_MESSAGE_BEHAVIOR_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Timestamps,
        slug: "timestamps",
        label: "Timestamps",
        keywords: &[
            "timestamp",
            "clock",
            "time format",
            "hide timestamps while live",
            "hide timestamps when live",
            "12 hour",
            "24 hour",
            "seconds",
        ],
        settings: CHAT_TIMESTAMPS_SETTINGS,
    },
];

const EMOTES_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::EmotesDisplay,
        slug: "display",
        label: "Display",
        keywords: &[
            "emoji style",
            "twemoji",
            "noto",
            "native emoji",
            "emote spacing",
            "zero width",
            "overlay emote",
        ],
        settings: EMOTES_DISPLAY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::EmotesProviders,
        slug: "providers",
        label: "Providers",
        keywords: &[
            "emote provider",
            "7tv",
            "bttv",
            "ffz",
            "account emotes",
            "subscription",
            "channel emotes",
        ],
        settings: EMOTES_PROVIDERS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::EmotesCosmetics,
        slug: "cosmetics-badges",
        label: "Cosmetics & badges",
        keywords: &[
            "third party badges",
            "7tv cosmetics",
            "paint",
            "avatar",
            "cosmetic",
        ],
        settings: EMOTES_COSMETICS_SETTINGS,
    },
];

const HIGHLIGHTS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsPreviewColors,
        slug: "preview-colors",
        label: "Preview & colors",
        keywords: &[
            "live preview",
            "message highlights",
            "quick presets",
            "color",
            "mention color",
            "whisper color",
            "subscription color",
            "first message",
        ],
        settings: HIGHLIGHTS_PREVIEW_COLORS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsBehavior,
        slug: "behavior",
        label: "Behavior",
        keywords: &[
            "self messages",
            "own messages",
            "elevated",
            "hype chat",
            "reply thread",
            "mention behavior",
            "flash",
            "title",
        ],
        settings: HIGHLIGHTS_BEHAVIOR_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsCustomRules,
        slug: "custom-rules",
        label: "Custom rules",
        keywords: &[
            "custom highlight",
            "phrase",
            "regex",
            "ignored users",
            "blacklist",
            "badge highlight",
            "test highlights",
        ],
        settings: HIGHLIGHTS_CUSTOM_RULES_SETTINGS,
    },
];

const FILTERS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::FilterRules,
        slug: "filter-rules",
        label: "Filter rules",
        keywords: &["filter", "rule", "expression", "regex", "hide", "replace"],
        settings: FILTERS_RULES_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::BlockedUsers,
        slug: "blocked-users",
        label: "Blocked users",
        keywords: &["block", "blocked user", "unblock", "allow"],
        settings: FILTERS_BLOCKED_USERS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::SimilarMessages,
        slug: "similar-messages",
        label: "Similar messages",
        keywords: &[
            "chat",
            "spam",
            "duplicate",
            "similar",
            "similarity",
            "threshold",
            "repeated",
            "detection",
        ],
        settings: FILTERS_SIMILAR_MESSAGES_SETTINGS,
    },
];

const COMMANDS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::SlashCommands,
        slug: "slash-commands",
        label: "Slash commands",
        keywords: &[
            "slash command",
            "autocomplete",
            "description",
            "built in command",
        ],
        settings: COMMANDS_SLASH_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::CustomCommands,
        slug: "custom-commands",
        label: "Custom commands",
        keywords: &[
            "custom command",
            "trigger",
            "alias",
            "placeholder",
            "response",
            "import",
            "export",
        ],
        settings: COMMANDS_CUSTOM_SETTINGS,
    },
];

const MODERATION_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::UserCard,
        slug: "user-card",
        label: "User card",
        keywords: &[
            "user card",
            "usercard",
            "pronouns",
            "follow date",
            "follower count",
            "fields",
        ],
        settings: MODERATION_USER_CARD_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ModerationButtons,
        slug: "moderation-buttons",
        label: "Moderation buttons",
        keywords: &[
            "moderation button",
            "timeout",
            "ban",
            "delete",
            "destructive",
        ],
        settings: MODERATION_BUTTONS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ModerationVariables,
        slug: "variables",
        label: "Variables",
        keywords: &["variable", "message variables", "placeholder"],
        settings: MODERATION_VARIABLES_SETTINGS,
    },
];

const HOTKEYS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysNavigation,
        slug: "navigation",
        label: "Navigation",
        keywords: &["navigation", "tab", "split", "switch", "next", "previous"],
        settings: HOTKEYS_NAVIGATION_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysChat,
        slug: "chat",
        label: "Chat",
        keywords: &["chat", "send", "reply", "clear", "scroll"],
        settings: HOTKEYS_CHAT_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysSearch,
        slug: "search",
        label: "Search",
        keywords: &["search", "find", "jump"],
        settings: HOTKEYS_SEARCH_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysModeration,
        slug: "moderation",
        label: "Moderation",
        keywords: &["moderation", "timeout", "ban", "mod"],
        settings: HOTKEYS_MODERATION_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysOther,
        slug: "other",
        label: "Other",
        keywords: &["other", "misc", "window", "overlay"],
        settings: HOTKEYS_OTHER_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysCustom,
        slug: "custom-hotkeys",
        label: "Custom hotkeys",
        keywords: &[
            "custom hotkey",
            "custom command hotkey",
            "binding",
            "conflict",
        ],
        settings: HOTKEYS_CUSTOM_SETTINGS,
    },
];

const LINKS_MEDIA_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Links,
        slug: "links",
        label: "Links",
        keywords: &[
            "link",
            "browser",
            "custom browser",
            "incognito",
            "trusted domain",
            "confirmation",
        ],
        settings: LINKS_MEDIA_LINKS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::LinkPreviews,
        slug: "link-previews",
        label: "Link previews",
        keywords: &[
            "link preview",
            "embed",
            "autoplay",
            "thumbnail",
            "metadata",
            "privacy",
            "remote fetch",
        ],
        settings: LINKS_MEDIA_LINK_PREVIEWS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ImageUploads,
        slug: "image-uploads",
        label: "Image uploads",
        keywords: &[
            "image uploader",
            "upload",
            "imgur",
            "sharex",
            "request",
            "response mapping",
            "multipart",
            "headers",
            "upload history",
            "clipboard",
            "paste",
        ],
        settings: LINKS_MEDIA_IMAGE_UPLOADS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Players,
        slug: "players",
        label: "Players",
        keywords: &[
            "player",
            "custom player",
            "streamlink",
            "quality",
            "mpv",
            "vlc",
        ],
        settings: LINKS_MEDIA_PLAYERS_SETTINGS,
    },
];

const STREAMER_MODE_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::StreamerPrivacy,
        slug: "privacy",
        label: "Privacy",
        keywords: &[
            "streamer mode",
            "streaming software",
            "obs",
            "privacy options",
            "hide",
            "account names",
            "avatar",
            "link thumbnails",
            "viewer count",
        ],
        settings: STREAMER_MODE_PRIVACY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::StreamerNotifications,
        slug: "notifications",
        label: "Notifications",
        keywords: &[
            "notification",
            "whisper",
            "mention content",
            "mod actions",
            "automod terms",
        ],
        settings: STREAMER_MODE_NOTIFICATIONS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DataManagement,
        slug: "data-management",
        label: "Data management",
        keywords: &["data", "cache", "clear message cache", "reset", "storage"],
        settings: STREAMER_MODE_DATA_SETTINGS,
    },
];

const DEVELOPER_TOOLS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::NetworkProxy,
        slug: "network-proxy",
        label: "Network proxy",
        keywords: &[
            "proxy",
            "proxy mode",
            "manual proxy url",
            "network",
            "http",
            "socks",
        ],
        settings: DEVELOPER_NETWORK_PROXY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatLogging,
        slug: "chat-logging",
        label: "Chat logging",
        keywords: &[
            "log content",
            "readable channel logs",
            "deleted message display",
            "deleted message limit",
            "logging",
            "log file",
        ],
        settings: DEVELOPER_CHAT_LOGGING_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HistoryCleanup,
        slug: "history-cleanup",
        label: "History & cleanup",
        keywords: &[
            "message history",
            "cleanup",
            "retention",
            "latest history file",
            "directory",
        ],
        settings: DEVELOPER_HISTORY_CLEANUP_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Diagnostics,
        slug: "diagnostics",
        label: "Diagnostics",
        keywords: &[
            "diagnostics",
            "debug",
            "frame rate",
            "runtime liveness",
            "7tv eventapi",
            "message performance",
            "emote loading",
            "memory usage",
            "metrics",
        ],
        settings: DEVELOPER_DIAGNOSTICS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Configuration,
        slug: "configuration",
        label: "Configuration",
        keywords: &[
            "configuration",
            "settings file",
            "import",
            "export",
            "reset",
            "backup",
        ],
        settings: DEVELOPER_CONFIGURATION_SETTINGS,
    },
];

const LEGACY_SUBSECTION_TABLES: &[&[SettingsSubsectionDescriptor]] = &[
    APPEARANCE_SUBSECTIONS,
    NOTIFICATIONS_SUBSECTIONS,
    UPDATES_SUBSECTIONS,
    CHAT_SUBSECTIONS,
    EMOTES_SUBSECTIONS,
    HIGHLIGHTS_SUBSECTIONS,
    FILTERS_SUBSECTIONS,
    COMMANDS_SUBSECTIONS,
    MODERATION_SUBSECTIONS,
    HOTKEYS_SUBSECTIONS,
    LINKS_MEDIA_SUBSECTIONS,
    STREAMER_MODE_SUBSECTIONS,
    DEVELOPER_TOOLS_SUBSECTIONS,
];

fn legacy_subsection_by_slug(
    page_slug: &str,
    subsection_slug: &str,
) -> Option<&'static SettingsSubsectionDescriptor> {
    let subsections = match page_slug.trim().to_ascii_lowercase().as_str() {
        "appearance" | "theme" => APPEARANCE_SUBSECTIONS,
        "notifications" | "sounds" => NOTIFICATIONS_SUBSECTIONS,
        "updates" => UPDATES_SUBSECTIONS,
        "chat" | "chat-behavior" => CHAT_SUBSECTIONS,
        "emotes" => EMOTES_SUBSECTIONS,
        "highlights" => HIGHLIGHTS_SUBSECTIONS,
        "filters" | "similarity" => FILTERS_SUBSECTIONS,
        "commands" => COMMANDS_SUBSECTIONS,
        "moderation" => MODERATION_SUBSECTIONS,
        "hotkeys" => HOTKEYS_SUBSECTIONS,
        "links-media" | "connections" | "uploader" | "embeds" => LINKS_MEDIA_SUBSECTIONS,
        "streamer-mode" | "privacy" => STREAMER_MODE_SUBSECTIONS,
        "advanced" | "developer-tools" | "proxy" | "logging" | "debug" => {
            DEVELOPER_TOOLS_SUBSECTIONS
        }
        _ => return None,
    };
    subsections
        .iter()
        .find(|descriptor| descriptor.slug.eq_ignore_ascii_case(subsection_slug))
}

fn legacy_chat_subsection_slug(
    page_slug: &str,
    subsection_slug: &str,
) -> Option<SettingsSubsectionId> {
    if !(page_slug.eq_ignore_ascii_case("chat") || page_slug.eq_ignore_ascii_case("chat-behavior"))
    {
        return None;
    }
    if subsection_slug.eq_ignore_ascii_case("display")
        || subsection_slug.eq_ignore_ascii_case("chat-display")
    {
        return Some(SettingsSubsectionId::ChatDisplay);
    }
    if subsection_slug.eq_ignore_ascii_case("providers") {
        return Some(SettingsSubsectionId::EmotesProviders);
    }
    if subsection_slug.eq_ignore_ascii_case("cosmetics-badges") {
        return Some(SettingsSubsectionId::EmotesCosmetics);
    }
    if subsection_slug.eq_ignore_ascii_case("nicknames") {
        return Some(SettingsSubsectionId::NicknameRules);
    }
    None
}

const GENERAL_STREAMER_KEYWORDS: &[&str] = &[
    "streamer mode",
    "streaming software",
    "obs",
    "privacy options",
    "hide",
    "account names",
    "avatar",
    "link thumbnails",
    "viewer count",
    "notification",
    "notifications",
    "whisper",
    "mention content",
    "mod actions",
    "automod terms",
];

const GENERAL_UPDATE_KEYWORDS: &[&str] = &[
    "update",
    "auto update",
    "check for updates",
    "download",
    "install",
    "restart",
    "signature",
    "update origin",
    "current release",
    "version",
    "release",
    "changelog",
    "build",
];

const GENERAL_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Account,
        slug: "account",
        label: "Account",
        keywords: LEGACY_PAGE_KEYWORDS[0].1,
        settings: ACCOUNT_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::StreamerPrivacy,
        slug: "privacy",
        label: "Streamer mode",
        keywords: GENERAL_STREAMER_KEYWORDS,
        settings: STREAMER_MODE_GENERAL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::UpdateChecks,
        slug: "update-checks",
        label: "Update checks",
        keywords: GENERAL_UPDATE_KEYWORDS,
        settings: UPDATES_GENERAL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::SettingsWindow,
        slug: "settings-window",
        label: "Window & startup",
        keywords: APPEARANCE_SUBSECTIONS[3].keywords,
        settings: GENERAL_SETTINGS_WINDOW_SETTINGS,
    },
];

const FLAT_APPEARANCE_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ThemeColors,
        slug: "theme-colors",
        label: "Theme & colors",
        keywords: APPEARANCE_SUBSECTIONS[0].keywords,
        settings: APPEARANCE_THEME_PRIMARY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::AppearanceMotion,
        slug: "motion",
        label: "Motion",
        keywords: &["motion", "animation", "reduce"],
        settings: APPEARANCE_MOTION_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::TabsSizing,
        slug: "tabs-sizing",
        label: "Tabs & sizing",
        keywords: APPEARANCE_SUBSECTIONS[2].keywords,
        settings: APPEARANCE_TABS_PRIMARY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::AppearanceOptions,
        slug: "appearance-options",
        label: "More appearance options",
        keywords: APPEARANCE_SUBSECTIONS[3].keywords,
        settings: APPEARANCE_OPTIONS_SETTINGS,
    },
];

const FLAT_CHAT_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatReading,
        slug: "reading",
        label: "Reading",
        keywords: &["reading"],
        settings: CHAT_READING_ALL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatComposing,
        slug: "composing",
        label: "Composing",
        keywords: CHAT_SUBSECTIONS[0].keywords,
        settings: CHAT_COMPOSING_ALL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::EmotesDisplay,
        slug: "emotes",
        label: "Emotes",
        keywords: EMOTES_SUBSECTIONS[0].keywords,
        settings: CHAT_EMOTES_ALL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatOptions,
        slug: "chat-options",
        label: "More chat options",
        keywords: &[
            "more",
            "chat options",
            "interaction",
            "scrolling",
            "message display",
        ],
        settings: CHAT_OPTIONS_SETTINGS,
    },
];

const FLAT_HIGHLIGHTS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsBehavior,
        slug: "behavior",
        label: "Attention",
        keywords: HIGHLIGHTS_SUBSECTIONS[1].keywords,
        settings: HIGHLIGHTS_BEHAVIOR_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsCustomRules,
        slug: "custom-rules",
        label: "Highlight rules",
        keywords: HIGHLIGHTS_SUBSECTIONS[2].keywords,
        settings: HIGHLIGHTS_CUSTOM_RULES_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HighlightsPreviewColors,
        slug: "preview-colors",
        label: "Palette",
        keywords: HIGHLIGHTS_SUBSECTIONS[0].keywords,
        settings: HIGHLIGHTS_PREVIEW_COLORS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Sounds,
        slug: "sounds",
        label: "Sounds",
        keywords: NOTIFICATIONS_SUBSECTIONS[0].keywords,
        settings: NOTIFICATIONS_SOUNDS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DesktopNotifications,
        slug: "desktop-notifications",
        label: "Notifications",
        keywords: NOTIFICATIONS_SUBSECTIONS[1].keywords,
        settings: NOTIFICATIONS_PRIMARY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::MutedChannels,
        slug: "muted-channels",
        label: "Muted channels",
        keywords: NOTIFICATIONS_SUBSECTIONS[3].keywords,
        settings: NOTIFICATIONS_MUTED_SETTINGS,
    },
];

const FLAT_MODERATION_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::UserCard,
        slug: "user-card",
        label: "User cards",
        keywords: MODERATION_SUBSECTIONS[0].keywords,
        settings: MODERATION_USER_CARD_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ModerationButtons,
        slug: "moderation-buttons",
        label: "Moderation buttons",
        keywords: MODERATION_SUBSECTIONS[1].keywords,
        settings: MODERATION_BUTTONS_ALL_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DeletedMessages,
        slug: "deleted-messages",
        label: "Deleted messages",
        keywords: &["delete", "clearmsg", "removed"],
        settings: MODERATION_DELETED_MESSAGES_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::FilterRules,
        slug: "filter-rules",
        label: "Filter rules",
        keywords: FILTERS_SUBSECTIONS[0].keywords,
        settings: FILTERS_RULES_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::BlockedUsers,
        slug: "blocked-users",
        label: "Blocked users",
        keywords: FILTERS_SUBSECTIONS[1].keywords,
        settings: FILTERS_BLOCKED_USERS_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::SimilarMessages,
        slug: "similar-messages",
        label: "Similar messages",
        keywords: FILTERS_SUBSECTIONS[2].keywords,
        settings: FILTERS_SIMILAR_MESSAGES_SETTINGS,
    },
];

const FLAT_SHORTCUTS_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::SlashCommands,
        slug: "slash-commands",
        label: "Commands",
        keywords: COMMANDS_SUBSECTIONS[0].keywords,
        settings: COMMANDS_SLASH_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::CustomCommands,
        slug: "custom-commands",
        label: "Custom commands",
        keywords: COMMANDS_SUBSECTIONS[1].keywords,
        settings: COMMANDS_CUSTOM_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HotkeysNavigation,
        slug: "navigation",
        label: "Keyboard shortcuts",
        keywords: &[
            "navigation",
            "tab",
            "split",
            "switch",
            "next",
            "previous",
            "chat",
            "send",
            "reply",
            "clear",
            "scroll",
            "search",
            "find",
            "jump",
            "moderation",
            "timeout",
            "ban",
            "mod",
            "other",
            "misc",
            "window",
            "overlay",
            "custom hotkey",
            "custom command hotkey",
            "binding",
            "conflict",
        ],
        settings: SHORTCUTS_ALL_SETTINGS,
    },
];

const FLAT_ADVANCED_SUBSECTIONS: &[SettingsSubsectionDescriptor] = &[
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::ChatLogging,
        slug: "chat-logging",
        label: "History & logs",
        keywords: DEVELOPER_TOOLS_SUBSECTIONS[1].keywords,
        settings: DEVELOPER_CHAT_LOGGING_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::HistoryCleanup,
        slug: "history-cleanup",
        label: "History files",
        keywords: DEVELOPER_TOOLS_SUBSECTIONS[2].keywords,
        settings: DEVELOPER_HISTORY_CLEANUP_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::NetworkProxy,
        slug: "network-proxy",
        label: "Network proxy",
        keywords: DEVELOPER_TOOLS_SUBSECTIONS[0].keywords,
        settings: DEVELOPER_NETWORK_PROXY_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Configuration,
        slug: "configuration",
        label: "Configuration",
        keywords: DEVELOPER_TOOLS_SUBSECTIONS[4].keywords,
        settings: DEVELOPER_CONFIGURATION_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DataManagement,
        slug: "data-management",
        label: "Data management",
        keywords: STREAMER_MODE_SUBSECTIONS[2].keywords,
        settings: STREAMER_MODE_DATA_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::DeveloperMode,
        slug: "developer-mode",
        label: "Developer mode",
        keywords: &["developer mode", "diagnostics access"],
        settings: super::sections::ADVANCED_DEVELOPER_MODE_SETTINGS,
    },
    SettingsSubsectionDescriptor {
        id: SettingsSubsectionId::Diagnostics,
        slug: "diagnostics",
        label: "Developer diagnostics",
        keywords: DEVELOPER_TOOLS_SUBSECTIONS[3].keywords,
        settings: DEVELOPER_DIAGNOSTICS_SETTINGS,
    },
];

const LEGACY_PAGE_KEYWORDS: &[(SettingsSectionId, &[&str])] = &[
    (
        SettingsSectionId::General,
        &[
            "account",
            "login",
            "logout",
            "anonymous",
            "profile",
            "twitch",
            "switch account",
            "remove account",
            "reauthenticate",
            "current account",
            "all accounts",
        ],
    ),
    (
        SettingsSectionId::Theme,
        &[
            "theme",
            "dark",
            "light",
            "color",
            "accent",
            "font",
            "scale",
            "tab",
            "size",
            "density",
            "spacing",
            "appearance",
            "username",
            "display name",
            "localized",
            "localization",
            "name style",
            "gray names",
            "colorize users",
            "badge",
            "window",
            "transparent",
            "overlay",
            "always on top",
            "click through",
            "ghost mode",
            "opacity",
            "idle fade",
            "font scale",
            "text size",
            "emote size",
            "image quality",
            "unread indicator",
            "tab overflow",
            "background tint",
            "theme file",
            "settings",
            "popout",
        ],
    ),
    (
        SettingsSectionId::Sounds,
        &[
            "sound",
            "audio",
            "notification",
            "desktop notification",
            "volume",
            "mute",
            "toast",
            "live channel",
            "online",
            "offline",
            "startup delay",
            "highlight sound",
        ],
    ),
    (
        SettingsSectionId::Updates,
        &[
            "update",
            "upgrade",
            "version",
            "release",
            "changelog",
            "download",
            "install",
            "restart",
            "update origin",
            "signature",
        ],
    ),
    (
        SettingsSectionId::ChatBehavior,
        &[
            "input",
            "send",
            "chat",
            "ascii",
            "pause",
            "character counter",
            "tab completion",
            "username completion",
            "mention comma",
            "unified input",
            "modifier",
            "username right click",
            "reply button",
            "mention action",
            "alternate click",
            "alt",
            "hover pause",
            "pause indicator",
            "split header",
            "one line header",
            "compact header",
            "timestamp",
        ],
    ),
    (
        SettingsSectionId::Emotes,
        &[
            "emote",
            "emoji",
            "scale",
            "spacing",
            "animation",
            "picker",
            "7tv",
            "paint",
            "badge",
            "bttv",
            "ffz",
            "provider",
            "avatar",
            "zero width",
            "subscription",
            "account emotes",
            "third party",
            "cosmetic",
            "twemoji",
            "noto",
            "native emoji",
        ],
    ),
    (
        SettingsSectionId::Highlights,
        &[
            "highlight",
            "mention",
            "color",
            "notification",
            "flash",
            "reply thread",
            "whisper",
            "subscription",
            "first message",
            "watch streak",
            "automod",
            "announcement",
            "raid",
            "bits",
            "cheer",
            "channel points",
            "hype chat",
            "own messages",
            "phrase",
            "ignored user",
            "blacklist",
        ],
    ),
    (
        SettingsSectionId::Filters,
        &[
            "filter",
            "block",
            "hide",
            "regex",
            "expression",
            "similar",
            "duplicate",
            "spam",
            "exclude own messages",
            "unblock",
            "allow",
            "replace",
        ],
    ),
    (
        SettingsSectionId::Nicknames,
        &[
            "nickname",
            "rename",
            "alias",
            "display",
            "name",
            "user",
            "regex",
            "case sensitive",
        ],
    ),
    (
        SettingsSectionId::Commands,
        &[
            "command",
            "custom",
            "slash",
            "trigger",
            "placeholder",
            "alias",
            "autocomplete",
            "description",
            "response",
            "import",
            "export",
        ],
    ),
    (
        SettingsSectionId::Moderation,
        &[
            "moderation",
            "mod",
            "timeout",
            "ban",
            "usercard",
            "buttons",
            "delete",
            "pronouns",
            "follow date",
            "follower count",
            "destructive",
            "message variables",
        ],
    ),
    (
        SettingsSectionId::Hotkeys,
        &[
            "hotkey",
            "shortcut",
            "keyboard",
            "keybind",
            "key",
            "binding",
            "custom command hotkey",
            "conflict",
            "reset defaults",
        ],
    ),
    (
        SettingsSectionId::Connections,
        &[
            "link",
            "media",
            "upload",
            "image",
            "clipboard",
            "paste",
            "imgur",
            "embed",
            "preview",
            "browser",
            "player",
            "streamlink",
            "incognito",
            "autoplay",
            "thumbnail",
            "metadata",
            "custom browser",
            "custom player",
            "quality",
            "sharex",
            "multipart",
            "headers",
        ],
    ),
    (
        SettingsSectionId::Privacy,
        &[
            "privacy",
            "streamer",
            "streaming",
            "obs",
            "hide",
            "avatar",
            "whisper",
            "data",
            "cache",
            "reset",
            "streaming software",
            "account names",
            "link thumbnails",
            "viewer count",
            "mod actions",
            "automod terms",
            "mention content",
            "clear message cache",
        ],
    ),
    (
        SettingsSectionId::Advanced,
        &[
            "advanced",
            "log",
            "history",
            "file",
            "directory",
            "debug",
            "developer",
            "experimental",
            "config",
            "import",
            "export",
            "reset",
            "proxy",
            "network",
            "http",
            "socks",
            "diagnostics",
            "metrics",
            "configuration",
        ],
    ),
];

const LEGACY_PAGE_DEFAULT_ANCHORS: &[(SettingsSectionId, SettingsSubsectionId)] = &[
    (SettingsSectionId::General, SettingsSubsectionId::Account),
    (SettingsSectionId::Theme, SettingsSubsectionId::ThemeColors),
    (SettingsSectionId::Sounds, SettingsSubsectionId::Sounds),
    (
        SettingsSectionId::Updates,
        SettingsSubsectionId::UpdateChecks,
    ),
    (
        SettingsSectionId::ChatBehavior,
        SettingsSubsectionId::ChatReading,
    ),
    (
        SettingsSectionId::Emotes,
        SettingsSubsectionId::EmotesDisplay,
    ),
    (
        SettingsSectionId::Highlights,
        SettingsSubsectionId::HighlightsPreviewColors,
    ),
    (
        SettingsSectionId::Filters,
        SettingsSubsectionId::SimilarMessages,
    ),
    (
        SettingsSectionId::Nicknames,
        SettingsSubsectionId::ChatOptions,
    ),
    (
        SettingsSectionId::Commands,
        SettingsSubsectionId::SlashCommands,
    ),
    (
        SettingsSectionId::Moderation,
        SettingsSubsectionId::UserCard,
    ),
    (
        SettingsSectionId::Hotkeys,
        SettingsSubsectionId::HotkeysNavigation,
    ),
    (
        SettingsSectionId::Connections,
        SettingsSubsectionId::ImageUploads,
    ),
    (
        SettingsSectionId::Privacy,
        SettingsSubsectionId::StreamerPrivacy,
    ),
    (
        SettingsSectionId::Advanced,
        SettingsSubsectionId::NetworkProxy,
    ),
];

const SECTIONS: &[SettingsSectionDescriptor] = &[
    SettingsSectionDescriptor {
        id: SettingsSectionId::General,
        slug: "general",
        legacy_slugs: &["account", "updates", "streamer-mode", "privacy"],
        label: "General",
        icon: "icons/settings.svg",
        keywords: &[
            "account",
            "update",
            "streamer",
            "privacy",
            "settings window",
        ],
        default_subsection: Some(SettingsSubsectionId::Account),
        subsections: GENERAL_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Theme,
        slug: "appearance",
        legacy_slugs: &["theme"],
        label: "Appearance",
        icon: "icons/palette.svg",
        keywords: &[
            "theme",
            "dark",
            "light",
            "color",
            "accent",
            "font",
            "scale",
            "tab",
            "size",
            "density",
            "spacing",
            "appearance",
            "username",
            "display name",
            "localized",
            "localization",
            "name style",
            "gray names",
            "colorize users",
            "badge",
            "window",
            "transparent",
            "overlay",
            "always on top",
            "click through",
            "ghost mode",
            "opacity",
            "idle fade",
            "font scale",
            "text size",
            "emote size",
            "image quality",
            "unread indicator",
            "tab overflow",
            "background tint",
            "theme file",
            "settings",
            "popout",
        ],
        default_subsection: Some(SettingsSubsectionId::ThemeColors),
        subsections: FLAT_APPEARANCE_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::ChatBehavior,
        slug: "chat",
        legacy_slugs: &[
            "chat-behavior",
            "emotes",
            "nicknames",
            "display",
            "chat-display",
            "input-composing",
            "message-behavior",
            "timestamps",
            "reading",
            "composing",
            "chat-options",
            "providers",
            "cosmetics-badges",
        ],
        label: "Chat",
        icon: "icons/message-square.svg",
        keywords: &[
            "input",
            "send",
            "chat",
            "ascii",
            "pause",
            "character counter",
            "tab completion",
            "username completion",
            "mention comma",
            "unified input",
            "modifier",
            "username right click",
            "reply button",
            "mention action",
            "alternate click",
            "alt",
            "hover pause",
            "pause indicator",
            "split header",
            "one line header",
            "compact header",
            "timestamp",
        ],
        default_subsection: Some(SettingsSubsectionId::ChatReading),
        subsections: FLAT_CHAT_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Highlights,
        slug: "highlights-notifications",
        legacy_slugs: &["highlights", "notifications", "sounds"],
        label: "Highlights & notifications",
        icon: "icons/sparkles.svg",
        keywords: &[
            "highlight",
            "mention",
            "color",
            "notification",
            "flash",
            "reply thread",
            "whisper",
            "subscription",
            "first message",
            "watch streak",
            "automod",
            "announcement",
            "raid",
            "bits",
            "cheer",
            "channel points",
            "hype chat",
            "own messages",
            "phrase",
            "ignored user",
            "blacklist",
        ],
        default_subsection: Some(SettingsSubsectionId::HighlightsBehavior),
        subsections: FLAT_HIGHLIGHTS_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Moderation,
        slug: "moderation-filters",
        legacy_slugs: &["moderation", "filters", "similarity"],
        label: "Moderation & filters",
        icon: "icons/shield.svg",
        keywords: &[
            "moderation",
            "mod",
            "timeout",
            "ban",
            "usercard",
            "buttons",
            "delete",
            "pronouns",
            "follow date",
            "follower count",
            "destructive",
            "message variables",
        ],
        default_subsection: Some(SettingsSubsectionId::UserCard),
        subsections: FLAT_MODERATION_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Hotkeys,
        slug: "shortcuts-commands",
        legacy_slugs: &["hotkeys", "commands"],
        label: "Shortcuts & commands",
        icon: "icons/keyboard.svg",
        keywords: &[
            "hotkey",
            "shortcut",
            "keyboard",
            "keybind",
            "key",
            "binding",
            "custom command hotkey",
            "conflict",
            "reset defaults",
        ],
        default_subsection: Some(SettingsSubsectionId::SlashCommands),
        subsections: FLAT_SHORTCUTS_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Connections,
        slug: "links-media",
        legacy_slugs: &["connections", "uploader", "embeds"],
        label: "Links & media",
        icon: "icons/link.svg",
        keywords: &[
            "link",
            "media",
            "upload",
            "image",
            "clipboard",
            "paste",
            "imgur",
            "embed",
            "preview",
            "browser",
            "player",
            "streamlink",
            "incognito",
            "autoplay",
            "thumbnail",
            "metadata",
            "custom browser",
            "custom player",
            "quality",
            "sharex",
            "multipart",
            "headers",
        ],
        default_subsection: Some(SettingsSubsectionId::Links),
        subsections: LINKS_MEDIA_SUBSECTIONS,
        settings: &[],
    },
    SettingsSectionDescriptor {
        id: SettingsSectionId::Advanced,
        slug: "advanced",
        legacy_slugs: &["developer-tools", "proxy", "logging", "debug"],
        label: "Advanced",
        icon: "icons/terminal.svg",
        keywords: &[
            "advanced",
            "log",
            "history",
            "file",
            "directory",
            "debug",
            "developer",
            "experimental",
            "config",
            "import",
            "export",
            "reset",
            "proxy",
            "network",
            "http",
            "socks",
            "diagnostics",
            "metrics",
            "configuration",
        ],
        default_subsection: Some(SettingsSubsectionId::ChatLogging),
        subsections: FLAT_ADVANCED_SUBSECTIONS,
        settings: &[],
    },
];

const SETTINGS_CATEGORY_SECTIONS: &[SettingsSectionId] = &[
    SettingsSectionId::General,
    SettingsSectionId::Theme,
    SettingsSectionId::ChatBehavior,
    SettingsSectionId::Highlights,
    SettingsSectionId::Moderation,
    SettingsSectionId::Connections,
    SettingsSectionId::Hotkeys,
    SettingsSectionId::Advanced,
];

/// Every category in tree order. The developer disclosure only appends the last
/// entry, so both shapes are sub-slices of one table rather than two copies.
const CATEGORIES: &[SettingsCategoryDescriptor] = &[SettingsCategoryDescriptor {
    id: SettingsCategoryId::General,
    slug: "settings",
    label: "Settings",
    sections: SETTINGS_CATEGORY_SECTIONS,
    visibility: SettingsVisibility::Always,
}];

/// Number of leading categories that are always disclosed. Keeping the
/// developer category last lets both shapes share one table.
const STANDARD_CATEGORY_COUNT: usize = 1;

/// Slugs from earlier layouts, including the ones that merged into a specific
/// anchor of a surviving page.
pub const LEGACY_SLUGS: &[LegacySlugMapping] = &[
    LegacySlugMapping {
        slug: "account",
        section: SettingsSectionId::General,
        subsection: Some(SettingsSubsectionId::Account),
    },
    LegacySlugMapping {
        slug: "updates",
        section: SettingsSectionId::General,
        subsection: Some(SettingsSubsectionId::UpdateChecks),
    },
    LegacySlugMapping {
        slug: "streamer-mode",
        section: SettingsSectionId::General,
        subsection: Some(SettingsSubsectionId::StreamerPrivacy),
    },
    LegacySlugMapping {
        slug: "privacy",
        section: SettingsSectionId::General,
        subsection: Some(SettingsSubsectionId::StreamerPrivacy),
    },
    LegacySlugMapping {
        slug: "theme",
        section: SettingsSectionId::Theme,
        subsection: Some(SettingsSubsectionId::ThemeColors),
    },
    LegacySlugMapping {
        slug: "chat-behavior",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::InputComposing),
    },
    LegacySlugMapping {
        slug: "display",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::ChatDisplay),
    },
    LegacySlugMapping {
        slug: "chat-display",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::ChatDisplay),
    },
    LegacySlugMapping {
        slug: "input-composing",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::InputComposing),
    },
    LegacySlugMapping {
        slug: "message-behavior",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::MessageBehavior),
    },
    LegacySlugMapping {
        slug: "timestamps",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::Timestamps),
    },
    LegacySlugMapping {
        slug: "reading",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::ChatReading),
    },
    LegacySlugMapping {
        slug: "composing",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::ChatComposing),
    },
    LegacySlugMapping {
        slug: "chat-options",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::ChatOptions),
    },
    LegacySlugMapping {
        slug: "emotes",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::EmotesDisplay),
    },
    LegacySlugMapping {
        slug: "providers",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::EmotesProviders),
    },
    LegacySlugMapping {
        slug: "cosmetics-badges",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::EmotesCosmetics),
    },
    LegacySlugMapping {
        slug: "nicknames",
        section: SettingsSectionId::ChatBehavior,
        subsection: Some(SettingsSubsectionId::NicknameRules),
    },
    LegacySlugMapping {
        slug: "highlights",
        section: SettingsSectionId::Highlights,
        subsection: Some(SettingsSubsectionId::HighlightsPreviewColors),
    },
    LegacySlugMapping {
        slug: "notifications",
        section: SettingsSectionId::Highlights,
        subsection: Some(SettingsSubsectionId::Sounds),
    },
    LegacySlugMapping {
        slug: "sounds",
        section: SettingsSectionId::Highlights,
        subsection: Some(SettingsSubsectionId::Sounds),
    },
    LegacySlugMapping {
        slug: "connections",
        section: SettingsSectionId::Connections,
        subsection: None,
    },
    LegacySlugMapping {
        slug: "moderation",
        section: SettingsSectionId::Moderation,
        subsection: Some(SettingsSubsectionId::UserCard),
    },
    LegacySlugMapping {
        slug: "uploader",
        section: SettingsSectionId::Connections,
        subsection: Some(SettingsSubsectionId::ImageUploads),
    },
    LegacySlugMapping {
        slug: "embeds",
        section: SettingsSectionId::Connections,
        subsection: Some(SettingsSubsectionId::LinkPreviews),
    },
    LegacySlugMapping {
        slug: "proxy",
        section: SettingsSectionId::Advanced,
        subsection: Some(SettingsSubsectionId::NetworkProxy),
    },
    LegacySlugMapping {
        slug: "logging",
        section: SettingsSectionId::Advanced,
        subsection: Some(SettingsSubsectionId::ChatLogging),
    },
    LegacySlugMapping {
        slug: "debug",
        section: SettingsSectionId::Advanced,
        subsection: Some(SettingsSubsectionId::Diagnostics),
    },
    LegacySlugMapping {
        slug: "developer-tools",
        section: SettingsSectionId::Advanced,
        subsection: Some(SettingsSubsectionId::NetworkProxy),
    },
    LegacySlugMapping {
        slug: "filters",
        section: SettingsSectionId::Moderation,
        subsection: Some(SettingsSubsectionId::FilterRules),
    },
    LegacySlugMapping {
        slug: "similarity",
        section: SettingsSectionId::Moderation,
        subsection: Some(SettingsSubsectionId::SimilarMessages),
    },
    LegacySlugMapping {
        slug: "commands",
        section: SettingsSectionId::Hotkeys,
        subsection: Some(SettingsSubsectionId::SlashCommands),
    },
    LegacySlugMapping {
        slug: "hotkeys",
        section: SettingsSectionId::Hotkeys,
        subsection: Some(SettingsSubsectionId::HotkeysNavigation),
    },
];

/// Categories in tree order, honouring the developer disclosure.
///
/// Also the single place the debug descriptor-integrity check runs, so a broken
/// table trips on the first settings render of a debug build instead of silently
/// corrupting navigation state.
pub fn build_categories(include_advanced: bool) -> &'static [SettingsCategoryDescriptor] {
    #[cfg(debug_assertions)]
    {
        use std::sync::Once;
        static VALIDATED: Once = Once::new();
        VALIDATED.call_once(|| {
            if let Err(issues) = validate_navigation_descriptors() {
                panic!(
                    "settings navigation descriptors are inconsistent:\n{}",
                    issues.join("\n")
                );
            }
        });
    }

    if include_advanced {
        CATEGORIES
    } else {
        &CATEGORIES[..STANDARD_CATEGORY_COUNT]
    }
}

pub fn category_descriptor(id: SettingsCategoryId) -> Option<&'static SettingsCategoryDescriptor> {
    CATEGORIES.iter().find(|category| category.id == id)
}

pub fn section_descriptor(id: SettingsSectionId) -> Option<&'static SettingsSectionDescriptor> {
    let id = canonical_section(id);
    SECTIONS.iter().find(|section| section.id == id)
}

pub fn subsection_descriptor(
    _section: SettingsSectionId,
    subsection: SettingsSubsectionId,
) -> Option<&'static SettingsSubsectionDescriptor> {
    let subsection = canonical_subsection(subsection);
    SECTIONS
        .iter()
        .find_map(|section| section.subsection(subsection))
}

pub fn subsection_owner(subsection: SettingsSubsectionId) -> Option<SettingsSectionId> {
    let subsection = canonical_subsection(subsection);
    SECTIONS
        .iter()
        .find(|section| section.subsection(subsection).is_some())
        .map(|section| section.id)
}

pub const fn subsection_is_developer_only(subsection: SettingsSubsectionId) -> bool {
    matches!(subsection, SettingsSubsectionId::Diagnostics)
}

/// The category that owns a page. Descriptor validation guarantees exactly one.
pub fn category_of_section(id: SettingsSectionId) -> Option<&'static SettingsCategoryDescriptor> {
    let id = canonical_section(id);
    CATEGORIES
        .iter()
        .find(|category| category.sections.contains(&id))
}

pub fn section_visibility(id: SettingsSectionId) -> SettingsVisibility {
    category_of_section(id)
        .map(|category| category.visibility)
        .unwrap_or_default()
}

pub fn is_section_visible(id: SettingsSectionId, developer_visible: bool) -> bool {
    section_visibility(id).is_visible(developer_visible)
}

pub fn section_slug(id: SettingsSectionId) -> &'static str {
    section_descriptor(id)
        .map(|section| section.slug)
        .unwrap_or("account")
}

/// Label of a destination, or `"Settings"` when the id is not currently
/// disclosed. Pointer-cheap lookup over the static table.
pub fn section_label(selected: SettingsSectionId, include_advanced: bool) -> &'static str {
    build_categories(include_advanced)
        .iter()
        .flat_map(|category| category.section_descriptors())
        .find(|section| section.id == canonical_section(selected))
        .map(|section| section.label)
        .unwrap_or("Settings")
}

pub fn subsection_label(
    section: SettingsSectionId,
    subsection: SettingsSubsectionId,
) -> Option<&'static str> {
    subsection_descriptor(section, subsection).map(|descriptor| descriptor.label)
}

/// `Category › Page › Anchor` breadcrumb parts. The category is included even
/// when the header omits it visually, because the accessible label needs it.
pub fn location_breadcrumb(location: SettingsLocation) -> Vec<&'static str> {
    let location = canonical_location(location);
    let mut parts = Vec::with_capacity(3);
    if let Some(category) = category_of_section(location.section) {
        parts.push(category.label);
    }
    if let Some(section) = section_descriptor(location.section) {
        parts.push(section.label);
    }
    if let Some(label) = location
        .subsection
        .and_then(|id| subsection_label(location.section, id))
    {
        parts.push(label);
    }
    parts
}

/// Resolve a page slug, accepting current and legacy spellings.
pub fn resolve_section_slug(slug: &str) -> Option<SettingsLocation> {
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }

    if let Some(section) = SECTIONS
        .iter()
        .find(|section| section.slug.eq_ignore_ascii_case(slug))
    {
        return Some(SettingsLocation::page(section.id));
    }

    normalize_legacy_slug(slug)
}

/// Resolve a live open request while retaining a legacy anchor until the
/// surface has revealed the disclosure that now owns it. Persisted locations
/// continue through [`resolve_slug_path`], which always canonicalizes.
pub fn resolve_requested_section_slug(slug: &str) -> Option<SettingsLocation> {
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }
    if let Some(section) = SECTIONS
        .iter()
        .find(|section| section.slug.eq_ignore_ascii_case(slug))
    {
        return Some(SettingsLocation::page(section.id));
    }
    legacy_slug_location(slug)
}

/// Legacy slug normalization table lookup (`uploader` -> Links & Media ›
/// Image uploads, ...).
pub fn normalize_legacy_slug(slug: &str) -> Option<SettingsLocation> {
    legacy_slug_location(slug).map(canonical_location)
}

fn legacy_slug_location(slug: &str) -> Option<SettingsLocation> {
    LEGACY_SLUGS
        .iter()
        .find(|mapping| mapping.slug.eq_ignore_ascii_case(slug.trim()))
        .map(|mapping| SettingsLocation {
            section: mapping.section,
            subsection: mapping.subsection,
        })
}

/// Resolve a persisted slug pair with the spec's fallback rules:
///
/// - unknown subsection on a known page -> that page's default anchor
/// - page hidden by the developer disclosure -> `None`
/// - unknown page -> `None` (caller falls back to the last valid location, then
///   Account)
pub fn resolve_slug_path(
    section_slug: &str,
    subsection_slug: Option<&str>,
    developer_visible: bool,
) -> Option<SettingsLocation> {
    let resolved = resolve_section_slug(section_slug)?;
    if !is_section_visible(resolved.section, developer_visible) {
        return None;
    }

    let section = section_descriptor(resolved.section)?;
    let subsection = match subsection_slug {
        Some(slug) => legacy_subsection_by_slug(section_slug, slug)
            .map(|descriptor| descriptor.id)
            .or_else(|| legacy_chat_subsection_slug(section_slug, slug))
            .or_else(|| {
                section
                    .subsection_by_slug(slug)
                    .map(|descriptor| descriptor.id)
            })
            .or_else(|| {
                LEGACY_SUBSECTION_TABLES
                    .iter()
                    .flat_map(|table| table.iter())
                    .filter(|descriptor| {
                        subsection_owner(canonical_subsection(descriptor.id)) == Some(section.id)
                    })
                    .find(|descriptor| descriptor.slug.eq_ignore_ascii_case(slug))
                    .map(|descriptor| descriptor.id)
            })
            .or(section.default_subsection),
        None => resolved.subsection.or(section.default_subsection),
    };

    let location = canonical_location(SettingsLocation {
        section: section.id,
        subsection,
    });
    Some(
        if !location.is_visible(developer_visible) && location.is_developer_only() {
            SettingsLocation::anchor(
                SettingsSectionId::Advanced,
                SettingsSubsectionId::DeveloperMode,
            )
        } else {
            location
        },
    )
}

/// Last-resort destination: first open, unknown page with no valid history.
pub fn default_location() -> SettingsLocation {
    SettingsLocation::page(SettingsSectionId::General)
}

pub fn developer_default_location() -> SettingsLocation {
    SettingsLocation::anchor(
        SettingsSectionId::Advanced,
        SettingsSubsectionId::Diagnostics,
    )
}

/// Restore the remembered standard location, or Account on first open / after a
/// removed page.
pub fn restore_location(
    prefs: &SettingsNavigationPrefs,
    developer_visible: bool,
) -> SettingsLocation {
    prefs
        .last_standard
        .as_ref()
        .and_then(|persisted| SettingsLocation::from_persisted(persisted, developer_visible))
        .filter(|location| !location.is_developer_only())
        .unwrap_or_else(default_location)
}

/// Restore the separately remembered developer location. Only meaningful while
/// the developer disclosure is on.
pub fn restore_developer_location(prefs: &SettingsNavigationPrefs) -> SettingsLocation {
    prefs
        .last_developer
        .as_ref()
        .and_then(|persisted| SettingsLocation::from_persisted(persisted, true))
        .filter(|location| location.section == SettingsSectionId::Advanced)
        .unwrap_or_else(developer_default_location)
}

/// Clamp an in-flight location to something that currently exists and is
/// disclosed. Falls back to the remembered standard location, then Account.
pub fn coerce_visible_location(
    location: SettingsLocation,
    prefs: &SettingsNavigationPrefs,
    developer_visible: bool,
) -> SettingsLocation {
    let location = canonical_location(location);
    if !developer_visible && location.is_developer_only() {
        return SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::DeveloperMode,
        );
    }
    if let Some(section) = section_descriptor(location.section)
        && location.is_visible(developer_visible)
    {
        let subsection = location
            .subsection
            .filter(|id| section.subsection(*id).is_some())
            .or(section.default_subsection);
        return SettingsLocation {
            section: section.id,
            subsection,
        };
    }

    restore_location(prefs, developer_visible)
}

/// Persisted navigation preferences.
///
/// The interaction fixture is a synthetic profile — it seeds its own tabs and
/// channels and asserts that driving the whole settings surface leaves the real
/// settings model byte-identical. Reading the user's stored tree state there
/// would make "default tree state" depend on whatever the developer last
/// expanded, so the fixture sees the first-open defaults instead.
pub fn navigation_prefs() -> SettingsNavigationPrefs {
    if crate::benchmark_ui_fixture::should_suppress_persistence() {
        return SettingsNavigationPrefs::default();
    }
    crate::backend::settings::get_settings_navigation_prefs()
}

/// Remembered location for a fresh open of either surface.
pub fn persisted_location(developer_visible: bool) -> SettingsLocation {
    restore_location(&navigation_prefs(), developer_visible)
}

/// Normalize an in-flight location before choosing which remembered slot owns
/// it. A hidden diagnostics anchor is a scroll position, not a developer-mode
/// choice, so retain the ordinary Developer mode access row instead.
pub fn persistence_location(
    location: SettingsLocation,
    developer_visible: bool,
) -> SettingsLocation {
    let location = presentation_location(location);
    if !developer_visible && location.is_developer_only() {
        return SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::DeveloperMode,
        );
    }
    location
}

/// Legacy anchors whose controls now share a primary group.
pub fn presentation_location(location: SettingsLocation) -> SettingsLocation {
    canonical_location(location)
}

#[derive(Clone, Copy)]
pub(crate) enum SettingsNavigationIntent {
    Browse,
    Search,
    OpenRequest,
}

pub(crate) fn disclosure_anchor(
    location: SettingsLocation,
    intent: SettingsNavigationIntent,
    query: &str,
) -> Option<SettingsLocation> {
    match intent {
        SettingsNavigationIntent::Browse => None,
        SettingsNavigationIntent::Search => Some(search_reveal_location(location, query)),
        SettingsNavigationIntent::OpenRequest => requested_anchor(location),
    }
}

/// Disclosures opened by an explicit legacy anchor request, never by scroll-spy.
pub fn anchor_disclosures(location: SettingsLocation) -> &'static [&'static str] {
    match location.subsection {
        Some(SettingsSubsectionId::StreamerNotifications) => &["general-protection-details"],
        Some(SettingsSubsectionId::WindowsOverlay) => &["appearance-more-windows"],
        Some(SettingsSubsectionId::InputComposing) => &["chat-more-composing"],
        Some(SettingsSubsectionId::MessageBehavior)
        | Some(SettingsSubsectionId::Timestamps)
        | Some(SettingsSubsectionId::ChatDisplay) => &["chat-more-reading"],
        Some(SettingsSubsectionId::EmotesProviders)
        | Some(SettingsSubsectionId::EmotesCosmetics) => &["chat-more-emotes"],
        Some(SettingsSubsectionId::NicknameRules) => &["chat-nicknames"],
        Some(SettingsSubsectionId::HighlightsBehavior) => &["highlights-behavior"],
        Some(SettingsSubsectionId::HighlightsPreviewColors) => &["highlights-colors"],
        Some(SettingsSubsectionId::HighlightsCustomRules) => &["highlights-rules"],
        Some(SettingsSubsectionId::Sounds) => &["sounds-events"],
        Some(SettingsSubsectionId::LiveChannels) => &["notifications-more"],
        Some(SettingsSubsectionId::MutedChannels) => &["notifications-muted"],
        Some(SettingsSubsectionId::ModerationButtons)
        | Some(SettingsSubsectionId::ModerationVariables) => &["moderation-buttons-editor"],
        Some(SettingsSubsectionId::FilterRules) => &["filters-rules-editor"],
        Some(SettingsSubsectionId::BlockedUsers) => &["filters-blocked-users"],
        Some(SettingsSubsectionId::SimilarMessages) => &["filters-similarity-options"],
        Some(SettingsSubsectionId::Links) => &["links-more"],
        Some(SettingsSubsectionId::LinkPreviews) => &["previews-more"],
        Some(SettingsSubsectionId::ImageUploads) => &["uploads-more"],
        Some(SettingsSubsectionId::Players) => &["players-editor"],
        Some(SettingsSubsectionId::NetworkProxy) => &["advanced-proxy-editor"],
        Some(SettingsSubsectionId::Configuration) => &[
            "advanced-configuration-files",
            "advanced-settings-import",
            "advanced-chatterino-import",
        ],
        Some(SettingsSubsectionId::ChatLogging) => &["advanced-log-options", "advanced-log-viewer"],
        Some(SettingsSubsectionId::HistoryCleanup) => &["advanced-history-files"],
        Some(SettingsSubsectionId::Diagnostics) => &["advanced-diagnostics"],
        Some(SettingsSubsectionId::CustomCommands) => &["commands-editor"],
        Some(SettingsSubsectionId::HotkeysNavigation)
        | Some(SettingsSubsectionId::HotkeysChat)
        | Some(SettingsSubsectionId::HotkeysSearch)
        | Some(SettingsSubsectionId::HotkeysModeration)
        | Some(SettingsSubsectionId::HotkeysOther)
        | Some(SettingsSubsectionId::HotkeysCustom) => &["shortcuts-editor"],
        _ => &[],
    }
}

pub fn anchor_disclosures_for_query(
    location: SettingsLocation,
    query: &str,
) -> &'static [&'static str] {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        let settings = match location.subsection {
            Some(SettingsSubsectionId::ChatLogging) => DEVELOPER_CHAT_LOGGING_SETTINGS,
            Some(SettingsSubsectionId::HistoryCleanup) => DEVELOPER_HISTORY_CLEANUP_SETTINGS,
            Some(SettingsSubsectionId::NetworkProxy) => DEVELOPER_NETWORK_PROXY_SETTINGS,
            Some(SettingsSubsectionId::Configuration) => DEVELOPER_CONFIGURATION_SETTINGS,
            Some(SettingsSubsectionId::Diagnostics) => DEVELOPER_DIAGNOSTICS_SETTINGS,
            _ => &[],
        };
        if let Some((_, setting)) = settings
            .iter()
            .filter_map(|setting| {
                text_rank(setting.label, setting.aliases, &query).map(|rank| (rank, setting))
            })
            .min_by_key(|(rank, _)| *rank)
        {
            return setting.disclosures;
        }
        if !settings.is_empty() {
            return if location.subsection == Some(SettingsSubsectionId::Diagnostics) {
                &["advanced-diagnostics"]
            } else {
                &[]
            };
        }
    }
    if location.subsection == Some(SettingsSubsectionId::ImageUploads) && !query.is_empty() {
        let matched = LINKS_MEDIA_IMAGE_UPLOADS_SETTINGS
            .iter()
            .filter_map(|setting| {
                text_rank(setting.label, setting.aliases, &query).map(|rank| (rank, setting))
            })
            .min_by_key(|(rank, _)| *rank);
        if let Some((_, setting)) = matched
            && !setting.disclosures.is_empty()
        {
            return setting.disclosures;
        }
    }
    anchor_disclosures(location)
}

/// Retain the legacy anchor that caused a search result to match, when it owns
/// a disclosure. The visible result still uses its current physical block, but
/// activation needs the old identity to reveal the newly consolidated details.
pub fn search_reveal_location(location: SettingsLocation, query: &str) -> SettingsLocation {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return location;
    }
    let current = canonical_location(location);
    let Some(current_anchor) = current.subsection else {
        return location;
    };

    for legacy in LEGACY_SUBSECTION_TABLES
        .iter()
        .flat_map(|table| table.iter())
    {
        if canonical_subsection(legacy.id) != current_anchor
            || text_rank(legacy.label, legacy.keywords, &query).is_none()
        {
            continue;
        }
        let reveal = SettingsLocation::anchor(current.section, legacy.id);
        if !anchor_disclosures(reveal).is_empty() {
            return reveal;
        }
    }

    for (legacy_page, default_anchor) in LEGACY_PAGE_DEFAULT_ANCHORS {
        if canonical_subsection(*default_anchor) != current_anchor {
            continue;
        }
        let Some((_, keywords)) = LEGACY_PAGE_KEYWORDS
            .iter()
            .find(|(page, _)| page == legacy_page)
        else {
            continue;
        };
        if text_rank("", keywords, &query).is_some() {
            let reveal = SettingsLocation::anchor(*legacy_page, *default_anchor);
            if !anchor_disclosures(reveal).is_empty() {
                return reveal;
            }
        }
    }

    location
}

/// Persist a navigation destination. Developer pages are remembered separately
/// so turning the disclosure off restores the last standard page instead.
/// Returns whether anything changed.
pub fn persist_location(location: SettingsLocation) -> bool {
    if crate::benchmark_ui_fixture::should_suppress_persistence() {
        return false;
    }
    let persisted = location.to_persisted();
    if location.is_developer_only() {
        crate::backend::settings::set_settings_navigation_last_developer(Some(persisted))
    } else {
        crate::backend::settings::set_settings_navigation_last_standard(Some(persisted))
    }
}

/// Validate the static tables: unique ids/slugs, every page in exactly one
/// category, resolvable defaults, and a legacy table that agrees with the page
/// descriptors. Runs once per process in debug builds via [`build_categories`].
pub fn validate_navigation_descriptors() -> Result<(), Vec<String>> {
    let mut issues = Vec::new();

    for (index, category) in CATEGORIES.iter().enumerate() {
        if CATEGORIES[..index]
            .iter()
            .any(|other| other.id == category.id)
        {
            issues.push(format!("duplicate category id {:?}", category.id));
        }
        if CATEGORIES[..index]
            .iter()
            .any(|other| other.slug == category.slug)
        {
            issues.push(format!("duplicate category slug '{}'", category.slug));
        }
        if category.sections.is_empty() {
            issues.push(format!("category '{}' has no pages", category.slug));
        }
    }

    for (index, category) in CATEGORIES.iter().enumerate() {
        let hidden = category.visibility.is_developer_only();
        if hidden != (index >= STANDARD_CATEGORY_COUNT) {
            issues.push(format!(
                "category '{}' visibility does not match STANDARD_CATEGORY_COUNT",
                category.slug
            ));
        }
    }

    for (index, section) in SECTIONS.iter().enumerate() {
        if SECTIONS[..index].iter().any(|other| other.id == section.id) {
            issues.push(format!("duplicate page id {:?}", section.id));
        }
        if SECTIONS[..index]
            .iter()
            .any(|other| other.slug == section.slug)
        {
            issues.push(format!("duplicate page slug '{}'", section.slug));
        }

        let owners = CATEGORIES
            .iter()
            .filter(|category| category.sections.contains(&section.id))
            .count();
        if owners != 1 {
            issues.push(format!(
                "page '{}' appears in {owners} categories (expected exactly 1)",
                section.slug
            ));
        }

        match section.default_subsection {
            Some(default) if section.subsection(default).is_none() => issues.push(format!(
                "page '{}' default anchor {default:?} is not one of its anchors",
                section.slug
            )),
            None if !section.subsections.is_empty() => issues.push(format!(
                "page '{}' has anchors but no default anchor",
                section.slug
            )),
            _ => {}
        }

        for (sub_index, subsection) in section.subsections.iter().enumerate() {
            if section.subsections[..sub_index]
                .iter()
                .any(|other| other.slug == subsection.slug)
            {
                issues.push(format!(
                    "duplicate anchor slug '{}' on page '{}'",
                    subsection.slug, section.slug
                ));
            }
            let global = SECTIONS
                .iter()
                .flat_map(|other| other.subsections.iter())
                .filter(|other| other.id == subsection.id)
                .count();
            if global != 1 {
                issues.push(format!(
                    "anchor id {:?} is used {global} times (expected exactly 1)",
                    subsection.id
                ));
            }
        }

        if !section.settings.is_empty() && !section.subsections.is_empty() {
            issues.push(format!(
                "page '{}' declares page-level settings but has anchors to hang them on",
                section.slug
            ));
        }

        for legacy in section.legacy_slugs {
            if !LEGACY_SLUGS
                .iter()
                .any(|mapping| mapping.slug == *legacy && mapping.section == section.id)
            {
                issues.push(format!(
                    "page '{}' declares legacy slug '{legacy}' that is missing from LEGACY_SLUGS",
                    section.slug
                ));
            }
        }
    }

    {
        let all_settings = || {
            SECTIONS.iter().flat_map(|section| {
                section.settings.iter().chain(
                    section
                        .subsections
                        .iter()
                        .flat_map(|subsection| subsection.settings.iter()),
                )
            })
        };
        for setting in all_settings() {
            if setting.id.is_empty() || setting.control_id.is_empty() {
                issues.push(format!("setting '{}' has an empty target id", setting.id));
            }
            for (index, id) in setting.disclosures.iter().enumerate() {
                if id.is_empty() || setting.disclosures[..index].contains(id) {
                    issues.push(format!(
                        "setting '{}' has an invalid disclosure chain",
                        setting.id
                    ));
                }
            }
            if setting.label.is_empty() {
                issues.push(format!("setting '{}' has an empty label", setting.id));
            }
            let by_id = all_settings()
                .filter(|other| other.id == setting.id)
                .count();
            if by_id != 1 {
                issues.push(format!(
                    "setting id '{}' is declared {by_id} times (expected exactly 1)",
                    setting.id
                ));
            }
            let by_control = all_settings()
                .filter(|other| other.control_id == setting.control_id)
                .count();
            if by_control != 1 {
                issues.push(format!(
                    "control id '{}' is declared {by_control} times (expected exactly 1)",
                    setting.control_id
                ));
            }
        }
    }

    for (index, mapping) in LEGACY_SLUGS.iter().enumerate() {
        if LEGACY_SLUGS[..index]
            .iter()
            .any(|other| other.slug == mapping.slug)
        {
            issues.push(format!("duplicate legacy slug '{}'", mapping.slug));
        }
        if SECTIONS.iter().any(|section| section.slug == mapping.slug) {
            issues.push(format!(
                "legacy slug '{}' shadows a current page slug",
                mapping.slug
            ));
        }
        match section_descriptor(mapping.section) {
            None => issues.push(format!(
                "legacy slug '{}' targets unknown page {:?}",
                mapping.slug, mapping.section
            )),
            Some(section) => {
                if !section.legacy_slugs.contains(&mapping.slug) {
                    issues.push(format!(
                        "legacy slug '{}' is not listed on page '{}'",
                        mapping.slug, section.slug
                    ));
                }
                if let Some(subsection) = mapping.subsection.map(canonical_subsection)
                    && section.subsection(subsection).is_none()
                {
                    issues.push(format!(
                        "legacy slug '{}' targets anchor {subsection:?} that page '{}' does not have",
                        mapping.slug, section.slug
                    ));
                }
            }
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Case-insensitive substring test against an already-lowercased needle.
/// The descriptor tables are ASCII, so this avoids allocating a lowercased copy
/// of every label on every keystroke.
fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Rank a label/keyword pair against a query that has already been trimmed and
/// lowercased once for the whole pass. An empty query matches everything.
fn text_rank(label: &str, keywords: &[&str], query_lower: &str) -> Option<u8> {
    if query_lower.is_empty() {
        return Some(0);
    }

    if label.eq_ignore_ascii_case(query_lower) {
        return Some(0);
    }
    if contains_ignore_ascii_case(label, query_lower) {
        return Some(1);
    }
    if keywords
        .iter()
        .any(|keyword| contains_ignore_ascii_case(keyword, query_lower))
    {
        return Some(2);
    }

    query_lower
        .split_whitespace()
        .all(|token| {
            contains_ignore_ascii_case(label, token)
                || keywords
                    .iter()
                    .any(|keyword| contains_ignore_ascii_case(keyword, token))
        })
        .then_some(3)
}

/// Rank a destination against a query. Kept as a free function because the
/// legacy flat sidebar orders results by this rank.
fn section_search_rank(section: &SettingsSectionDescriptor, query_lower: &str) -> Option<u8> {
    text_rank(section.label, section.keywords, query_lower)
}

fn subsection_search_rank(
    subsection: &SettingsSubsectionDescriptor,
    query_lower: &str,
) -> Option<u8> {
    let current = text_rank(subsection.label, subsection.keywords, query_lower);
    let legacy_keywords = LEGACY_SUBSECTION_TABLES
        .iter()
        .flat_map(|table| table.iter())
        .filter(|legacy| canonical_subsection(legacy.id) == subsection.id)
        .filter_map(|legacy| text_rank("", legacy.keywords, query_lower));
    let renamed_labels = LEGACY_SUBSECTION_TABLES
        .iter()
        .flat_map(|table| table.iter())
        .filter(|legacy| {
            legacy.id == subsection.id && !legacy.label.eq_ignore_ascii_case(subsection.label)
        })
        .filter_map(|legacy| text_rank(legacy.label, &[], query_lower));
    let legacy_pages = LEGACY_PAGE_DEFAULT_ANCHORS
        .iter()
        .filter(|(_, anchor)| *anchor == subsection.id)
        .filter_map(|(legacy, _)| {
            LEGACY_PAGE_KEYWORDS
                .iter()
                .find(|(page, _)| page == legacy)
                .and_then(|(_, keywords)| text_rank("", keywords, query_lower))
        });
    current
        .into_iter()
        .chain(legacy_keywords)
        .chain(renamed_labels)
        .chain(legacy_pages)
        .min()
}

fn setting_matches(setting: &SettingDescriptor, query_lower: &str) -> bool {
    text_rank(setting.label, setting.aliases, query_lower).is_some()
}

/// Match tallies for one subtree. `settings` drives the count badges and the
/// `Role::Status` announcement ("7 settings in 3 subsections").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsSearchCounts {
    pub settings: usize,
    pub subsections: usize,
    pub pages: usize,
}

impl SettingsSearchCounts {
    fn merge(&mut self, other: SettingsSearchCounts) {
        self.settings += other.settings;
        self.subsections += other.subsections;
        self.pages += other.pages;
    }

    pub fn is_empty(self) -> bool {
        self.settings == 0 && self.subsections == 0 && self.pages == 0
    }
}

/// A retained anchor in the filtered tree.
#[derive(Clone, Debug)]
pub struct SettingsSearchSubsection {
    pub section: SettingsSectionId,
    pub descriptor: &'static SettingsSubsectionDescriptor,
    /// The anchor label/keywords matched directly.
    pub matched: bool,
    /// Settings under this anchor that matched, in declaration order.
    pub settings: Vec<&'static SettingDescriptor>,
    pub counts: SettingsSearchCounts,
}

impl SettingsSearchSubsection {
    pub fn location(&self) -> SettingsLocation {
        canonical_location(SettingsLocation::anchor(self.section, self.descriptor.id))
    }

    /// First matching setting label, used as anchor secondary text
    /// ("Hide timestamps when live · +2").
    pub fn first_match_label(&self) -> Option<&'static str> {
        self.settings.first().map(|setting| setting.label)
    }

    /// Extra matches beyond the first, for the `· +N` suffix.
    pub fn additional_match_count(&self) -> usize {
        self.settings.len().saturating_sub(1)
    }
}

/// A retained page in the filtered tree.
#[derive(Clone, Debug)]
pub struct SettingsSearchSection {
    pub descriptor: &'static SettingsSectionDescriptor,
    /// The page label/keywords matched directly.
    pub matched: bool,
    /// Relevance rank; lower sorts first. 4 marks a descendant-only match.
    pub rank: u8,
    pub subsections: Vec<SettingsSearchSubsection>,
    /// Page-level settings that matched. Only leaf pages ever have these; pages
    /// with anchors carry their matches on the anchors instead.
    pub settings: Vec<&'static SettingDescriptor>,
    pub counts: SettingsSearchCounts,
}

impl SettingsSearchSection {
    pub fn id(&self) -> SettingsSectionId {
        self.descriptor.id
    }

    /// First matching setting label, including one owned by an anchor.
    pub fn first_match_label(&self) -> Option<&'static str> {
        self.settings
            .first()
            .map(|setting| setting.label)
            .or_else(|| {
                self.subsections
                    .iter()
                    .find_map(SettingsSearchSubsection::first_match_label)
            })
    }

    /// Extra matching settings beyond the first, for the `· +N` suffix.
    pub fn additional_match_count(&self) -> usize {
        self.counts.settings.saturating_sub(1)
    }

    /// Where Enter on this row lands: the first matching anchor, else the page
    /// default.
    pub fn location(&self) -> SettingsLocation {
        match self
            .subsections
            .iter()
            .find(|sub| sub.counts.subsections > 0)
        {
            Some(subsection) => subsection.location(),
            None => self.descriptor.default_location(),
        }
    }
}

/// A retained category in the filtered tree.
#[derive(Clone, Debug)]
pub struct SettingsSearchCategory {
    pub descriptor: &'static SettingsCategoryDescriptor,
    pub matched: bool,
    pub sections: Vec<SettingsSearchSection>,
    pub counts: SettingsSearchCounts,
}

/// One filtered view of the static navigation tables.
///
/// Built once per query revision (see [`SettingsSearchState`]) and shared by the
/// tree, the legacy flat sidebar, the "no matching settings" state, the count
/// announcement, and the selected-label lookup, so a keystroke ranks the
/// hierarchy once instead of two or three times.
#[derive(Clone, Debug)]
pub struct SettingsSearchModel {
    /// Sidebar groups in render order. Normal browsing keeps one group per
    /// category. An active search collapses them into one relevance-ordered
    /// result list so an exact page-name hit is never buried below a looser
    /// keyword match in an earlier category.
    groups: Vec<(&'static str, Vec<&'static SettingsSection>)>,
    /// Relevance-ordered ids of every matching destination.
    ordered_ids: Vec<SettingsSectionId>,
    /// Ancestor-retaining tree for the three-level navigation surface.
    tree: Vec<SettingsSearchCategory>,
    targets: Vec<SettingTarget>,
    counts: SettingsSearchCounts,
    first_location: Option<SettingsLocation>,
    /// Matches that exist only behind the developer disclosure, so the surface
    /// can offer "Developer settings are hidden" instead of a wrong empty state.
    hidden_developer_counts: SettingsSearchCounts,
    hidden_developer_location: Option<SettingsLocation>,
    hidden_developer_targets: Vec<SettingTarget>,
    is_searching: bool,
}

impl SettingsSearchModel {
    pub fn compute(query: &str, include_advanced: bool) -> Self {
        let query_lower = query.trim().to_lowercase();
        let categories = build_categories(include_advanced);

        if query_lower.is_empty() {
            let tree: Vec<SettingsSearchCategory> = categories
                .iter()
                .map(|category| SettingsSearchCategory {
                    descriptor: category,
                    matched: false,
                    sections: category
                        .section_descriptors()
                        .map(|section| SettingsSearchSection {
                            descriptor: section,
                            matched: false,
                            rank: 0,
                            subsections: section
                                .subsections
                                .iter()
                                .map(|subsection| SettingsSearchSubsection {
                                    section: section.id,
                                    descriptor: subsection,
                                    matched: false,
                                    settings: Vec::new(),
                                    counts: SettingsSearchCounts::default(),
                                })
                                .collect(),
                            settings: Vec::new(),
                            counts: SettingsSearchCounts::default(),
                        })
                        .collect(),
                    counts: SettingsSearchCounts::default(),
                })
                .collect();

            return Self {
                groups: categories
                    .iter()
                    .map(|category| (category.label, category.section_descriptors().collect()))
                    .collect(),
                ordered_ids: categories
                    .iter()
                    .flat_map(|category| category.sections.iter().copied())
                    .collect(),
                tree,
                targets: Vec::new(),
                counts: SettingsSearchCounts::default(),
                first_location: None,
                hidden_developer_counts: SettingsSearchCounts::default(),
                hidden_developer_location: None,
                hidden_developer_targets: Vec::new(),
                is_searching: false,
            };
        }

        let (tree, counts, first_location) =
            Self::filter(categories, &query_lower, include_advanced);
        let targets = Self::collect_targets(&tree, &query_lower);

        let mut matches: Vec<(u8, &'static SettingsSectionDescriptor)> = tree
            .iter()
            .flat_map(|category| category.sections.iter())
            .map(|section| (section.rank, section.descriptor))
            .collect();
        matches.sort_by_key(|(rank, _)| *rank);

        let ordered_ids = matches.iter().map(|(_, section)| section.id).collect();
        let groups = if matches.is_empty() {
            Vec::new()
        } else {
            vec![(
                "Results",
                matches.into_iter().map(|(_, section)| section).collect(),
            )]
        };

        let hidden_developer_targets = if include_advanced {
            Vec::new()
        } else {
            setting_targets()
                .filter(|target| {
                    target.is_developer_only() && setting_matches(&target.setting, &query_lower)
                })
                .collect()
        };
        let hidden_developer_counts = SettingsSearchCounts {
            settings: hidden_developer_targets.len(),
            subsections: usize::from(!hidden_developer_targets.is_empty()),
            pages: usize::from(!hidden_developer_targets.is_empty()),
        };
        let hidden_developer_location = hidden_developer_targets
            .first()
            .map(|target| target.location);

        Self {
            groups,
            ordered_ids,
            tree,
            targets,
            counts,
            first_location,
            hidden_developer_counts,
            hidden_developer_location,
            hidden_developer_targets,
            is_searching: true,
        }
    }

    fn collect_targets(tree: &[SettingsSearchCategory], query: &str) -> Vec<SettingTarget> {
        let mut targets = Vec::new();
        for section in tree.iter().flat_map(|category| &category.sections) {
            targets.extend(section.settings.iter().map(|setting| SettingTarget {
                location: canonical_location(SettingsLocation::page(section.id())),
                setting: **setting,
            }));
            for subsection in &section.subsections {
                targets.extend(subsection.settings.iter().map(|setting| SettingTarget {
                    location: canonical_location(subsection.location()),
                    setting: **setting,
                }));
            }
        }
        targets.sort_by_key(|target| {
            text_rank(target.setting.label, target.setting.aliases, query).unwrap_or(4)
        });
        targets
    }

    pub fn targets(&self) -> &[SettingTarget] {
        &self.targets
    }

    /// Retain matching context before considering a result on another page.
    pub fn preferred_target(
        &self,
        location: SettingsLocation,
        selected: Option<&str>,
        restrict_to_location: bool,
    ) -> Option<SettingTarget> {
        let in_location = |target: &&SettingTarget| {
            target.location.section == location.section
                && (location.subsection.is_none()
                    || target.location.subsection == location.subsection)
        };
        let in_scope = self.targets.iter().filter(in_location);
        let target = in_scope
            .clone()
            .find(|target| selected == Some(target.row_id()))
            .or_else(|| in_scope.clone().next());
        if restrict_to_location {
            return target.copied();
        }
        target
            .or_else(|| {
                self.targets.iter().find(|target| {
                    target.location.section == location.section && selected == Some(target.row_id())
                })
            })
            .or_else(|| {
                self.targets
                    .iter()
                    .find(|target| target.location.section == location.section)
            })
            .or_else(|| self.targets.first())
            .copied()
    }

    pub fn hidden_developer_targets(&self) -> &[SettingTarget] {
        &self.hidden_developer_targets
    }

    /// Ancestor-retaining filter pass. A node that matches keeps its whole
    /// subtree; a node that only contains matches keeps just the matching path.
    fn filter(
        categories: &'static [SettingsCategoryDescriptor],
        query_lower: &str,
        include_developer: bool,
    ) -> (
        Vec<SettingsSearchCategory>,
        SettingsSearchCounts,
        Option<SettingsLocation>,
    ) {
        let mut tree = Vec::new();
        let mut totals = SettingsSearchCounts::default();
        let mut first_location = None;

        for category in categories {
            let mut sections = Vec::new();
            let mut category_counts = SettingsSearchCounts::default();

            for section in category.section_descriptors() {
                let rank = section_search_rank(section, query_lower);
                let section_matched = rank.is_some();
                let retain_all = section_matched;

                let mut subsections = Vec::new();
                let mut section_counts = SettingsSearchCounts::default();

                for subsection in section.subsections {
                    if subsection_is_developer_only(subsection.id) && !include_developer {
                        continue;
                    }
                    let subsection_matched =
                        subsection_search_rank(subsection, query_lower).is_some();
                    let matched_settings: Vec<&'static SettingDescriptor> = subsection
                        .settings
                        .iter()
                        .filter(|setting| setting_matches(setting, query_lower))
                        .collect();
                    let counted = subsection_matched || !matched_settings.is_empty();

                    if !counted && !retain_all {
                        continue;
                    }

                    let counts = SettingsSearchCounts {
                        settings: matched_settings.len(),
                        subsections: usize::from(counted),
                        pages: 0,
                    };
                    section_counts.merge(counts);
                    subsections.push(SettingsSearchSubsection {
                        section: section.id,
                        descriptor: subsection,
                        matched: subsection_matched,
                        settings: matched_settings,
                        counts,
                    });
                }

                let matched_page_settings: Vec<&'static SettingDescriptor> = section
                    .settings
                    .iter()
                    .filter(|setting| setting_matches(setting, query_lower))
                    .collect();
                section_counts.settings += matched_page_settings.len();

                let has_descendant_match =
                    section_counts.subsections > 0 || !matched_page_settings.is_empty();
                if !section_matched && !has_descendant_match {
                    continue;
                }

                section_counts.pages = 1;
                category_counts.merge(section_counts);

                let entry = SettingsSearchSection {
                    descriptor: section,
                    matched: section_matched,
                    rank: rank.unwrap_or(4),
                    subsections,
                    settings: matched_page_settings,
                    counts: section_counts,
                };
                if first_location.is_none() {
                    first_location = Some(entry.location());
                }
                sections.push(entry);
            }

            if sections.is_empty() {
                continue;
            }

            totals.merge(category_counts);
            tree.push(SettingsSearchCategory {
                descriptor: category,
                matched: false,
                sections,
                counts: category_counts,
            });
        }

        (tree, totals, first_location)
    }

    /// Legacy flat sidebar groups (`(header, sections)`).
    pub fn groups(&self) -> &[(&'static str, Vec<&'static SettingsSection>)] {
        &self.groups
    }

    /// Relevance-ordered ids of every matching destination.
    pub fn ordered_ids(&self) -> &[SettingsSectionId] {
        &self.ordered_ids
    }

    /// Ancestor-retaining tree for the navigation surface.
    pub fn tree(&self) -> &[SettingsSearchCategory] {
        &self.tree
    }

    pub fn has_matches(&self) -> bool {
        !self.ordered_ids.is_empty()
    }

    pub fn is_searching(&self) -> bool {
        self.is_searching
    }

    /// Match tallies for the whole result set.
    pub fn counts(&self) -> SettingsSearchCounts {
        self.counts
    }

    /// First matching destination, used when the current location stops
    /// matching.
    pub fn first_location(&self) -> Option<SettingsLocation> {
        self.first_location
    }

    /// Whether a destination survived the filter.
    pub fn contains_section(&self, id: SettingsSectionId) -> bool {
        self.ordered_ids.contains(&id)
    }

    /// Whether an exact location (page + anchor) survived the filter.
    pub fn contains_location(&self, location: SettingsLocation) -> bool {
        self.tree
            .iter()
            .flat_map(|category| category.sections.iter())
            .filter(|section| section.descriptor.id == location.section)
            .any(|section| match location.subsection {
                None => true,
                Some(id) => section
                    .subsections
                    .iter()
                    .any(|subsection| subsection.descriptor.id == id),
            })
    }

    /// Matches that exist only behind the developer disclosure.
    pub fn hidden_developer_counts(&self) -> SettingsSearchCounts {
        self.hidden_developer_counts
    }

    pub fn has_hidden_developer_matches(&self) -> bool {
        !self.hidden_developer_counts.is_empty()
    }

    /// First hidden developer destination, selected after the user reveals it.
    pub fn hidden_developer_location(&self) -> Option<SettingsLocation> {
        self.hidden_developer_location
    }

    /// Live-region text for the `Role::Status` node, e.g.
    /// `"7 settings in 3 subsections"`.
    pub fn status_announcement(&self) -> Option<String> {
        if !self.is_searching {
            return None;
        }
        if !self.has_matches() {
            return Some("No settings found".to_string());
        }

        let counts = self.counts;
        let pages = plural(counts.pages, "page", "pages");
        if counts.settings > 0 {
            let container = if counts.subsections > 0 {
                plural(counts.subsections, "subsection", "subsections")
            } else {
                pages.clone()
            };
            Some(format!(
                "{} in {container}",
                plural(counts.settings, "setting", "settings"),
            ))
        } else if counts.subsections > 0 {
            Some(format!(
                "{} in {pages}",
                plural(counts.subsections, "subsection", "subsections"),
            ))
        } else {
            Some(pages)
        }
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Revision-gated cache around [`SettingsSearchModel`].
///
/// Hosts keep one of these on their retained state and call [`Self::sync`] from
/// render; the model is rebuilt only when the query or the developer disclosure
/// actually changed, so a repaint caused by anything else reuses the previous
/// pass. `revision` is the fence for one-shot status/scroll tasks.
#[derive(Debug)]
pub struct SettingsSearchState {
    revision: u64,
    query: String,
    developer_visible: bool,
    model: SettingsSearchModel,
}

impl Default for SettingsSearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsSearchState {
    pub fn new() -> Self {
        Self {
            revision: 1,
            query: String::new(),
            developer_visible: false,
            model: SettingsSearchModel::compute("", false),
        }
    }

    /// Recompute only when the inputs changed. Returns whether a new model was
    /// built (i.e. whether visible state changed).
    pub fn sync(&mut self, query: &str, developer_visible: bool) -> bool {
        if self.query == query && self.developer_visible == developer_visible {
            return false;
        }

        self.query.clear();
        self.query.push_str(query);
        self.developer_visible = developer_visible;
        self.model = SettingsSearchModel::compute(query, developer_visible);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn model(&self) -> &SettingsSearchModel {
        &self.model
    }

    /// Monotonic fence for revision-gated one-shot tasks.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod exact_target_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn advanced_operations_and_developer_access_stay_visible_without_metrics() {
        let mode = setting_target_by_id("advanced.developer-mode").unwrap();
        assert!(!mode.is_developer_only());
        assert!(mode.location.is_visible(false));
        let diagnostics = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::Diagnostics,
        );
        assert!(!diagnostics.is_visible(false));
        assert!(diagnostics.is_visible(true));
        let gate = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::DeveloperMode,
        );
        assert_eq!(
            coerce_visible_location(diagnostics, &SettingsNavigationPrefs::default(), false),
            gate
        );
        assert_eq!(
            coerce_visible_location(diagnostics, &SettingsNavigationPrefs::default(), true),
            diagnostics
        );
        for page in ["advanced", "developer-tools", "debug"] {
            assert_eq!(
                resolve_slug_path(page, Some("diagnostics"), false),
                Some(gate)
            );
            assert_eq!(
                resolve_slug_path(page, Some("diagnostics"), true),
                Some(diagnostics)
            );
        }
        for id in [
            "developer-tools.proxy-mode",
            "developer-tools.proxy-url",
            "developer-tools.persist-history",
            "developer-tools.settings-import",
            "streamer-mode.reset-all-settings",
        ] {
            assert!(
                !setting_target_by_id(id).unwrap().is_developer_only(),
                "{id}"
            );
        }
        for descriptor in DEVELOPER_DIAGNOSTICS_SETTINGS {
            let target = setting_target_by_id(descriptor.id).unwrap();
            assert!(target.is_developer_only());
            assert_eq!(target.setting.disclosures, &["advanced-diagnostics"]);
        }
        assert!(
            SettingsSearchModel::compute("fps", false)
                .targets()
                .is_empty()
        );
    }

    #[test]
    fn advanced_search_reveals_only_the_owning_editor() {
        let logs = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::ChatLogging,
        );
        assert_eq!(
            anchor_disclosures_for_query(logs, "Write readable channel logs"),
            &[] as &[&str]
        );
        assert_eq!(
            anchor_disclosures_for_query(logs, "History & logs"),
            &[] as &[&str]
        );
        let config = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::Configuration,
        );
        assert_eq!(
            anchor_disclosures_for_query(config, "Configuration"),
            &[] as &[&str]
        );
        assert_eq!(
            anchor_disclosures_for_query(logs, "Server timestamps"),
            &["advanced-log-options"]
        );
        assert_eq!(
            anchor_disclosures_for_query(logs, "Search readable logs"),
            &["advanced-log-viewer"]
        );
        for id in [
            SettingsSubsectionId::ChatLogging,
            SettingsSubsectionId::NetworkProxy,
            SettingsSubsectionId::Configuration,
            SettingsSubsectionId::Diagnostics,
        ] {
            let location = SettingsLocation::anchor(SettingsSectionId::Advanced, id);
            assert!(disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none());
        }
    }

    #[test]
    fn historical_logging_slugs_keep_their_distinct_reveal_paths() {
        for page in ["advanced", "developer-tools", "logging", "proxy", "debug"] {
            for (slug, anchor, disclosures) in [
                (
                    "chat-logging",
                    SettingsSubsectionId::ChatLogging,
                    &["advanced-log-options", "advanced-log-viewer"] as &[&str],
                ),
                (
                    "history-cleanup",
                    SettingsSubsectionId::HistoryCleanup,
                    &["advanced-history-files"] as &[&str],
                ),
            ] {
                let location = resolve_slug_path(page, Some(slug), false).unwrap();
                assert_eq!(
                    location,
                    SettingsLocation::anchor(SettingsSectionId::Advanced, anchor),
                    "{page}/{slug}"
                );
                assert_eq!(anchor_disclosures(location), disclosures);
                assert!(
                    disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none()
                );
                assert_eq!(
                    disclosure_anchor(location, SettingsNavigationIntent::OpenRequest, ""),
                    Some(location)
                );
            }
        }
        for (id, disclosures) in [
            (
                "developer-tools.log-server-timestamps",
                &["advanced-log-options"] as &[&str],
            ),
            (
                "developer-tools.log-by-stream-id",
                &["advanced-log-options"],
            ),
            (
                "developer-tools.log-deleted-messages",
                &["advanced-log-options"],
            ),
            (
                "developer-tools.readable-log-format",
                &["advanced-log-options"],
            ),
            (
                "developer-tools.deleted-message-limit",
                &["advanced-log-options"],
            ),
            (
                "developer-tools.readable-log-search",
                &["advanced-log-viewer"],
            ),
            (
                "developer-tools.readable-log-search-scope-latest",
                &["advanced-log-viewer"],
            ),
            (
                "developer-tools.readable-log-search-scope-global",
                &["advanced-log-viewer"],
            ),
            (
                "developer-tools.history-folder",
                &["advanced-history-files"],
            ),
            (
                "developer-tools.latest-history-file",
                &["advanced-history-files"],
            ),
            (
                "developer-tools.history-cleanup",
                &["advanced-history-files"],
            ),
        ] {
            assert_eq!(
                setting_target_by_id(id).unwrap().setting.disclosures,
                disclosures,
                "{id}"
            );
        }
    }

    #[test]
    fn shortcut_actions_keep_exact_ids_and_editor_reveal_paths() {
        let mut actions = 0;
        for descriptor in SHORTCUTS_ALL_SETTINGS {
            let target = setting_target_by_id(descriptor.id)
                .unwrap_or_else(|| panic!("missing {}", descriptor.id));
            assert_eq!(target.setting.disclosures, &["shortcuts-editor"]);
            assert_eq!(
                target.location,
                SettingsLocation::anchor(
                    SettingsSectionId::Hotkeys,
                    SettingsSubsectionId::HotkeysNavigation,
                )
            );
            if descriptor.control_id.starts_with("hotkey-bind-") {
                actions += 1;
                let model = SettingsSearchModel::compute(descriptor.label, false);
                assert!(
                    model
                        .targets()
                        .iter()
                        .any(|found| found.row_id() == descriptor.id),
                    "{} is not searchable",
                    descriptor.id
                );
            }
        }
        assert_eq!(actions, 84);
        for descriptor in COMMANDS_CUSTOM_SETTINGS {
            assert_eq!(
                setting_target_by_id(descriptor.id)
                    .unwrap()
                    .setting
                    .disclosures,
                &["commands-editor"]
            );
        }
    }

    #[test]
    fn historical_shortcut_and_command_slug_pairs_keep_resolving() {
        for page in ["shortcuts-commands", "hotkeys", "commands"] {
            for slug in [
                "navigation",
                "chat",
                "search",
                "moderation",
                "other",
                "custom-hotkeys",
            ] {
                assert_eq!(
                    resolve_slug_path(page, Some(slug), false),
                    Some(SettingsLocation::anchor(
                        SettingsSectionId::Hotkeys,
                        SettingsSubsectionId::HotkeysNavigation
                    )),
                    "{page}/{slug}"
                );
            }
            for (slug, anchor) in [
                ("slash-commands", SettingsSubsectionId::SlashCommands),
                ("custom-commands", SettingsSubsectionId::CustomCommands),
            ] {
                assert_eq!(
                    resolve_slug_path(page, Some(slug), false),
                    Some(SettingsLocation::anchor(SettingsSectionId::Hotkeys, anchor)),
                    "{page}/{slug}"
                );
            }
        }
    }

    #[test]
    fn shortcut_browsing_does_not_reveal_the_editor() {
        for anchor in [
            SettingsSubsectionId::HotkeysNavigation,
            SettingsSubsectionId::HotkeysChat,
            SettingsSubsectionId::HotkeysSearch,
            SettingsSubsectionId::HotkeysModeration,
            SettingsSubsectionId::HotkeysOther,
            SettingsSubsectionId::HotkeysCustom,
        ] {
            let location = SettingsLocation::anchor(SettingsSectionId::Hotkeys, anchor);
            assert!(disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none());
            assert_eq!(anchor_disclosures(location), &["shortcuts-editor"]);
        }
    }

    #[test]
    fn registry_targets_are_unique_and_resolve() {
        assert_eq!(validate_navigation_descriptors(), Ok(()));
        for target in setting_targets() {
            assert_eq!(setting_target_by_id(target.row_id()), Some(target));
        }
        assert!(setting_target_by_id("missing.setting").is_none());
    }

    #[test]
    fn migrated_legacy_anchors_address_their_primary_group_and_details() {
        let notifications = SettingsLocation::anchor(
            SettingsSectionId::General,
            SettingsSubsectionId::StreamerNotifications,
        );
        assert_eq!(
            presentation_location(notifications),
            SettingsLocation::anchor(
                SettingsSectionId::General,
                SettingsSubsectionId::StreamerPrivacy
            )
        );
        assert_eq!(
            anchor_disclosures(notifications),
            &["general-protection-details"]
        );
        let release = SettingsLocation::anchor(
            SettingsSectionId::General,
            SettingsSubsectionId::CurrentRelease,
        );
        assert_eq!(
            presentation_location(release),
            SettingsLocation::anchor(
                SettingsSectionId::General,
                SettingsSubsectionId::UpdateChecks
            )
        );
        assert!(anchor_disclosures(SettingsLocation::page(SettingsSectionId::Theme)).is_empty());
        assert_eq!(
            anchor_disclosures(SettingsLocation::anchor(
                SettingsSectionId::Theme,
                SettingsSubsectionId::WindowsOverlay
            )),
            &["appearance-more-windows"]
        );
        assert_eq!(
            presentation_location(SettingsLocation::anchor(
                SettingsSectionId::Theme,
                SettingsSubsectionId::WindowsOverlay
            )),
            SettingsLocation::anchor(
                SettingsSectionId::Theme,
                SettingsSubsectionId::AppearanceOptions
            )
        );
        for (legacy, physical, disclosures) in [
            (
                SettingsSubsectionId::InputComposing,
                SettingsSubsectionId::ChatComposing,
                &["chat-more-composing"][..],
            ),
            (
                SettingsSubsectionId::MessageBehavior,
                SettingsSubsectionId::ChatReading,
                &["chat-more-reading"][..],
            ),
            (
                SettingsSubsectionId::Timestamps,
                SettingsSubsectionId::ChatReading,
                &["chat-more-reading"][..],
            ),
            (
                SettingsSubsectionId::ChatDisplay,
                SettingsSubsectionId::ChatReading,
                &["chat-more-reading"][..],
            ),
            (
                SettingsSubsectionId::EmotesProviders,
                SettingsSubsectionId::EmotesDisplay,
                &["chat-more-emotes"][..],
            ),
            (
                SettingsSubsectionId::EmotesCosmetics,
                SettingsSubsectionId::EmotesDisplay,
                &["chat-more-emotes"][..],
            ),
            (
                SettingsSubsectionId::NicknameRules,
                SettingsSubsectionId::ChatOptions,
                &["chat-nicknames"][..],
            ),
        ] {
            let location = SettingsLocation::anchor(SettingsSectionId::ChatBehavior, legacy);
            assert_eq!(
                presentation_location(location),
                SettingsLocation::anchor(SettingsSectionId::ChatBehavior, physical)
            );
            assert_eq!(anchor_disclosures(location), disclosures);
            let page = section_descriptor(SettingsSectionId::ChatBehavior).unwrap();
            let block = page.subsection(physical).unwrap();
            for disclosure in disclosures {
                assert!(
                    block
                        .settings
                        .iter()
                        .any(|setting| { setting.disclosures.contains(disclosure) })
                );
            }
        }
    }

    #[test]
    fn migrated_controls_keep_their_disclosure_paths() {
        for (id, disclosure, anchor) in [
            (
                "streamer-mode.hide-avatars",
                "general-protection-details",
                SettingsSubsectionId::StreamerPrivacy,
            ),
            (
                "streamer-mode.mute-mention-sounds",
                "general-protection-details",
                SettingsSubsectionId::StreamerPrivacy,
            ),
            (
                "appearance.primary-text",
                "appearance-more-colors",
                SettingsSubsectionId::AppearanceOptions,
            ),
            (
                "appearance.switch-color",
                "appearance-more-colors",
                SettingsSubsectionId::AppearanceOptions,
            ),
            (
                "appearance.badge-size",
                "appearance-more-display",
                SettingsSubsectionId::AppearanceOptions,
            ),
            (
                "appearance.tab-overflow",
                "appearance-more-tabs",
                SettingsSubsectionId::AppearanceOptions,
            ),
            (
                "appearance.window-transparent",
                "appearance-more-windows",
                SettingsSubsectionId::AppearanceOptions,
            ),
        ] {
            let target = setting_target_by_id(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(target.setting.disclosures, &[disclosure]);
            assert_eq!(target.location.subsection, Some(anchor));
        }
    }

    #[test]
    fn migrated_pages_keep_their_primary_and_secondary_inventory() {
        for (page, total, primary) in [
            (SettingsSectionId::General, 17, 5),
            (SettingsSectionId::Theme, 31, 7),
            (SettingsSectionId::ChatBehavior, 47, 8),
        ] {
            let targets: Vec<_> = setting_targets()
                .filter(|target| target.location.section == page)
                .collect();
            assert_eq!(targets.len(), total);
            assert_eq!(
                targets
                    .iter()
                    .filter(|target| target.setting.disclosures.is_empty())
                    .count(),
                primary
            );
        }
    }

    #[test]
    fn chat_migration_keeps_primary_destinations_and_secondary_reveals() {
        for (id, anchor, disclosure) in [
            (
                "appearance.username-style",
                SettingsSubsectionId::ChatReading,
                None,
            ),
            (
                "chat.show-timestamps",
                SettingsSubsectionId::ChatReading,
                None,
            ),
            (
                "chat.unified-input",
                SettingsSubsectionId::ChatComposing,
                None,
            ),
            ("emotes.animate", SettingsSubsectionId::EmotesDisplay, None),
            (
                "chat.timestamp-format",
                SettingsSubsectionId::ChatReading,
                Some("chat-more-reading"),
            ),
            (
                "emotes.seventv",
                SettingsSubsectionId::EmotesDisplay,
                Some("chat-more-emotes"),
            ),
            (
                "emotes.ffz-badges",
                SettingsSubsectionId::EmotesDisplay,
                Some("chat-more-emotes"),
            ),
            (
                "nicknames.pattern",
                SettingsSubsectionId::ChatOptions,
                Some("chat-nicknames"),
            ),
        ] {
            let target = setting_target_by_id(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(target.location.subsection, Some(anchor));
            match disclosure {
                Some(disclosure) => assert!(target.setting.disclosures.contains(&disclosure)),
                None => assert!(target.setting.disclosures.is_empty()),
            }
        }
    }

    #[test]
    fn every_setting_label_reaches_its_exact_target() {
        for target in setting_targets() {
            let query = format!("  {}  ", target.setting.label.to_uppercase());
            let model = SettingsSearchModel::compute(&query, true);
            assert!(
                model.targets().contains(&target),
                "unreachable {}",
                target.row_id()
            );
        }
    }

    #[test]
    fn switch_color_aliases_reveal_the_color_options_target() {
        let expected = setting_target_by_id("appearance.switch-color").unwrap();
        for query in ["toggle color", "switch accent", "switch colour"] {
            let model = SettingsSearchModel::compute(query, false);
            assert!(model.targets().contains(&expected));
            assert_eq!(expected.setting.disclosures, &["appearance-more-colors"]);
            assert_eq!(
                expected.location.subsection,
                Some(SettingsSubsectionId::AppearanceOptions)
            );
        }
    }

    #[test]
    fn every_legacy_subsection_keyword_reaches_its_current_anchor() {
        for legacy in LEGACY_SUBSECTION_TABLES
            .iter()
            .flat_map(|table| table.iter())
        {
            let expected_anchor = canonical_subsection(legacy.id);
            for keyword in legacy.keywords {
                let model = SettingsSearchModel::compute(keyword, true);
                assert!(
                    model
                        .tree()
                        .iter()
                        .flat_map(|category| &category.sections)
                        .flat_map(|section| &section.subsections)
                        .any(|current| {
                            current.descriptor.id == expected_anchor && current.matched
                        }),
                    "{} is no longer reachable for {:?}",
                    keyword,
                    legacy.id,
                );
            }
        }
    }

    #[test]
    fn every_legacy_page_keyword_reaches_its_current_default_anchor() {
        for (legacy_page, keywords) in LEGACY_PAGE_KEYWORDS {
            let (_, expected_anchor) = LEGACY_PAGE_DEFAULT_ANCHORS
                .iter()
                .find(|(page, _)| page == legacy_page)
                .unwrap();
            for keyword in *keywords {
                let model = SettingsSearchModel::compute(keyword, true);
                assert!(
                    model
                        .tree()
                        .iter()
                        .flat_map(|category| &category.sections)
                        .flat_map(|section| &section.subsections)
                        .any(|current| {
                            current.descriptor.id == *expected_anchor && current.matched
                        }),
                    "{keyword} is no longer reachable for {legacy_page:?}",
                );
            }
        }
    }

    #[test]
    fn synthetic_category_never_retains_the_flat_sidebar() {
        for query in ["settings", "set", "in"] {
            let model = SettingsSearchModel::compute(query, false);
            assert!(model.tree().iter().all(|category| !category.matched));
            assert!(
                model
                    .tree()
                    .iter()
                    .flat_map(|category| &category.sections)
                    .map(|section| section.subsections.len())
                    .sum::<usize>()
                    < 48,
                "{query} retained every anchor"
            );
        }
    }

    #[test]
    fn developer_targets_are_explicitly_separate() {
        let id = "developer-tools.frame-rate-analysis";
        let hidden = SettingsSearchModel::compute("Frame Rate Analysis", false);
        assert!(!hidden.targets().iter().any(|target| target.row_id() == id));
        assert!(
            hidden
                .hidden_developer_targets()
                .iter()
                .any(|target| target.row_id() == id && target.is_developer_only())
        );
        let revealed = SettingsSearchModel::compute("Frame Rate Analysis", true);
        assert!(
            revealed
                .targets()
                .iter()
                .any(|target| target.row_id() == id)
        );
        assert!(revealed.hidden_developer_targets().is_empty());
    }

    #[test]
    fn page_only_match_does_not_invent_setting_matches() {
        let model = SettingsSearchModel::compute("appearance", false);
        for target in model.targets() {
            assert!(setting_matches(&target.setting, "appearance"));
        }
        assert!(model.contains_section(SettingsSectionId::Theme));
        let cleared = SettingsSearchModel::compute("", false);
        assert!(cleared.targets().is_empty());
        assert!(cleared.hidden_developer_targets().is_empty());
    }

    #[test]
    fn alias_comparison_ignores_case_for_full_and_multiword_queries() {
        assert_eq!(
            text_rank("unrelated", &["toggleSettings"], "togglesettings"),
            Some(2)
        );
        assert_eq!(
            text_rank("unrelated", &["Returning Chatter"], "returning chatter"),
            Some(2)
        );
        assert_eq!(
            text_rank("unrelated", &["Returning", "Chatter"], "returning chatter"),
            Some(3)
        );
        let model = SettingsSearchModel::compute("TOGGLESETTINGS", false);
        assert!(
            model
                .targets()
                .iter()
                .any(|target| target.row_id() == "hotkeys.toggleSettings")
        );
    }

    #[test]
    fn matching_current_page_wins_over_a_globally_better_target() {
        let target = setting_targets()
            .find(|target| target.setting.label == "Highlight rule sound file")
            .unwrap();
        let model = SettingsSearchModel::compute("sound", false);
        let global = model.targets().first().unwrap();
        assert_ne!(global.location.section, target.location.section);
        let preferred = model
            .preferred_target(target.location, Some(global.row_id()), false)
            .unwrap();
        assert_eq!(preferred.location.section, target.location.section);
        let model = SettingsSearchModel::compute("Tab Appearance", false);
        let requested =
            SettingsLocation::anchor(SettingsSectionId::Theme, SettingsSubsectionId::ThemeColors);
        assert!(model.preferred_target(requested, None, true).is_none());
        assert!(
            model
                .preferred_target(SettingsLocation::page(SettingsSectionId::Theme), None, true)
                .is_some()
        );
        assert!(
            model
                .preferred_target(
                    SettingsLocation::page(SettingsSectionId::General),
                    None,
                    true
                )
                .is_none()
        );
    }

    #[test]
    fn old_page_and_anchor_slugs_still_resolve() {
        for mapping in LEGACY_SLUGS {
            let location = resolve_section_slug(mapping.slug).expect("legacy destination");
            assert_eq!(
                location,
                canonical_location(SettingsLocation {
                    section: mapping.section,
                    subsection: mapping.subsection,
                })
            );
        }
    }

    #[test]
    fn historical_chat_slug_spellings_resolve_to_their_current_destinations() {
        for (slug, subsection, expected) in [
            ("display", None, SettingsSubsectionId::ChatReading),
            ("providers", None, SettingsSubsectionId::EmotesDisplay),
            (
                "cosmetics-badges",
                None,
                SettingsSubsectionId::EmotesDisplay,
            ),
            ("nicknames", None, SettingsSubsectionId::ChatOptions),
            ("chat-display", None, SettingsSubsectionId::ChatReading),
            ("input-composing", None, SettingsSubsectionId::ChatComposing),
            ("message-behavior", None, SettingsSubsectionId::ChatReading),
            ("timestamps", None, SettingsSubsectionId::ChatReading),
            ("emotes", None, SettingsSubsectionId::EmotesDisplay),
            ("chat-options", None, SettingsSubsectionId::ChatOptions),
            ("reading", None, SettingsSubsectionId::ChatReading),
            ("composing", None, SettingsSubsectionId::ChatComposing),
            ("chat", Some("display"), SettingsSubsectionId::ChatReading),
            (
                "chat",
                Some("providers"),
                SettingsSubsectionId::EmotesDisplay,
            ),
            (
                "chat",
                Some("cosmetics-badges"),
                SettingsSubsectionId::EmotesDisplay,
            ),
            ("chat", Some("nicknames"), SettingsSubsectionId::ChatOptions),
            (
                "chat",
                Some("chat-display"),
                SettingsSubsectionId::ChatReading,
            ),
            (
                "chat",
                Some("input-composing"),
                SettingsSubsectionId::ChatComposing,
            ),
            (
                "chat",
                Some("message-behavior"),
                SettingsSubsectionId::ChatReading,
            ),
            (
                "chat",
                Some("timestamps"),
                SettingsSubsectionId::ChatReading,
            ),
            ("chat", Some("emotes"), SettingsSubsectionId::EmotesDisplay),
            (
                "chat",
                Some("chat-options"),
                SettingsSubsectionId::ChatOptions,
            ),
            ("chat", Some("reading"), SettingsSubsectionId::ChatReading),
            (
                "chat",
                Some("composing"),
                SettingsSubsectionId::ChatComposing,
            ),
        ] {
            assert_eq!(
                resolve_slug_path(slug, subsection, false),
                Some(SettingsLocation::anchor(
                    SettingsSectionId::ChatBehavior,
                    expected
                )),
                "{slug}/{subsection:?}"
            );
        }
    }

    #[test]
    fn live_legacy_slug_requests_retain_their_disclosure_anchor() {
        for (slug, legacy, disclosure) in [
            (
                "nicknames",
                SettingsSubsectionId::NicknameRules,
                "chat-nicknames",
            ),
            (
                "providers",
                SettingsSubsectionId::EmotesProviders,
                "chat-more-emotes",
            ),
            (
                "timestamps",
                SettingsSubsectionId::Timestamps,
                "chat-more-reading",
            ),
        ] {
            let requested = resolve_requested_section_slug(slug).unwrap();
            assert_eq!(requested.subsection, Some(legacy));
            assert!(anchor_disclosures(requested).contains(&disclosure));
            assert_eq!(
                canonical_location(requested).section,
                SettingsSectionId::ChatBehavior
            );
        }
    }

    #[test]
    fn legacy_page_ids_keep_their_original_default_anchor() {
        let cases = [
            (
                SettingsSectionId::Sounds,
                SettingsSectionId::Highlights,
                SettingsSubsectionId::Sounds,
            ),
            (
                SettingsSectionId::Updates,
                SettingsSectionId::General,
                SettingsSubsectionId::UpdateChecks,
            ),
            (
                SettingsSectionId::Emotes,
                SettingsSectionId::ChatBehavior,
                SettingsSubsectionId::EmotesDisplay,
            ),
            (
                SettingsSectionId::Nicknames,
                SettingsSectionId::ChatBehavior,
                SettingsSubsectionId::ChatOptions,
            ),
            (
                SettingsSectionId::Filters,
                SettingsSectionId::Moderation,
                SettingsSubsectionId::FilterRules,
            ),
            (
                SettingsSectionId::Commands,
                SettingsSectionId::Hotkeys,
                SettingsSubsectionId::SlashCommands,
            ),
            (
                SettingsSectionId::Privacy,
                SettingsSectionId::General,
                SettingsSubsectionId::StreamerPrivacy,
            ),
        ];
        for (legacy, section, subsection) in cases {
            assert_eq!(
                canonical_location(SettingsLocation::page(legacy)),
                SettingsLocation::anchor(section, subsection),
            );
        }
    }

    #[test]
    fn browsing_never_reveals_disclosures_on_any_migrated_page() {
        for page in SECTIONS {
            let location = SettingsLocation::page(page.id);
            assert!(disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none());
            for anchor in page.subsections {
                let location = SettingsLocation::anchor(page.id, anchor.id);
                assert!(
                    disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none()
                );
            }
        }
        for (section, anchor, chain) in [
            (
                SettingsSectionId::Theme,
                SettingsSubsectionId::WindowsOverlay,
                &["appearance-more-windows"][..],
            ),
            (
                SettingsSectionId::ChatBehavior,
                SettingsSubsectionId::MessageBehavior,
                &["chat-more-reading"][..],
            ),
            (
                SettingsSectionId::Highlights,
                SettingsSubsectionId::HighlightsPreviewColors,
                &["highlights-colors"][..],
            ),
            (
                SettingsSectionId::Highlights,
                SettingsSubsectionId::Sounds,
                &["sounds-events"][..],
            ),
        ] {
            let location = SettingsLocation::anchor(section, anchor);
            assert!(disclosure_anchor(location, SettingsNavigationIntent::Browse, "").is_none());
            let requested =
                disclosure_anchor(location, SettingsNavigationIntent::OpenRequest, "").unwrap();
            assert_eq!(anchor_disclosures(requested), chain);
        }
        let palette = SettingsLocation::anchor(
            SettingsSectionId::Highlights,
            SettingsSubsectionId::HighlightsPreviewColors,
        );
        assert!(
            disclosure_anchor(
                palette,
                SettingsNavigationIntent::Search,
                "preview & colors"
            )
            .is_some()
        );
    }

    #[test]
    fn unknown_subsections_fall_back_within_the_requested_page() {
        for slug in ["notifications", "not-a-setting", "chat-options"] {
            assert_eq!(
                resolve_slug_path("highlights-notifications", Some(slug), false),
                Some(SettingsLocation::anchor(
                    SettingsSectionId::Highlights,
                    SettingsSubsectionId::HighlightsBehavior
                ))
            );
        }
        assert_eq!(
            resolve_slug_path("appearance", Some("chat-display"), false),
            Some(SettingsLocation::anchor(
                SettingsSectionId::ChatBehavior,
                SettingsSubsectionId::ChatReading
            ))
        );
    }

    #[test]
    fn legacy_labels_rank_only_same_anchor_renames_and_choose_the_best_match() {
        let palette = SettingsSubsectionDescriptor {
            id: SettingsSubsectionId::HighlightsPreviewColors,
            slug: "preview-colors",
            label: "Palette",
            keywords: &["preview & colors hint"],
            settings: &[],
        };
        assert_eq!(
            subsection_search_rank(&palette, "preview & colors"),
            Some(0)
        );
        let reading = SettingsSubsectionDescriptor {
            id: SettingsSubsectionId::ChatReading,
            slug: "reading",
            label: "Reading",
            keywords: &[],
            settings: &[],
        };
        assert_eq!(subsection_search_rank(&reading, "message behavior"), None);
        let updates = subsection_descriptor(
            SettingsSectionId::General,
            SettingsSubsectionId::UpdateChecks,
        )
        .unwrap();
        assert_eq!(subsection_search_rank(updates, "current release"), Some(2));
    }

    #[test]
    fn links_migration_keeps_exact_controls_and_disclosure_paths() {
        let targets: Vec<_> = setting_targets()
            .filter(|target| target.location.section == SettingsSectionId::Connections)
            .collect();
        assert_eq!(targets.len(), 32);
        for (id, anchor, chain) in [
            (
                "links-media.ask-before-opening-links",
                SettingsSubsectionId::Links,
                &[][..],
            ),
            (
                "links-media.browser-path",
                SettingsSubsectionId::Links,
                &["links-more"][..],
            ),
            (
                "links-media.link-preview-mode",
                SettingsSubsectionId::LinkPreviews,
                &[][..],
            ),
            (
                "links-media.embed-remote-fetch",
                SettingsSubsectionId::LinkPreviews,
                &["previews-more"][..],
            ),
            (
                "links-media.uploader-enabled",
                SettingsSubsectionId::ImageUploads,
                &[][..],
            ),
            (
                "links-media.uploader-ask-paste",
                SettingsSubsectionId::ImageUploads,
                &[][..],
            ),
            (
                "links-media.uploader-drag-drop",
                SettingsSubsectionId::ImageUploads,
                &["uploads-more"][..],
            ),
            (
                "links-media.uploader-request-url",
                SettingsSubsectionId::ImageUploads,
                &["uploads-custom-provider"][..],
            ),
            (
                "links-media.upload-history",
                SettingsSubsectionId::ImageUploads,
                &["uploads-history"][..],
            ),
            (
                "links-media.streamlink-path",
                SettingsSubsectionId::Players,
                &["players-editor"][..],
            ),
            (
                "links-media.streamlink-custom-quality",
                SettingsSubsectionId::Players,
                &["players-editor"][..],
            ),
        ] {
            let target = setting_target_by_id(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(target.location.subsection, Some(anchor));
            assert_eq!(target.setting.disclosures, chain);
        }
        assert_eq!(
            section_descriptor(SettingsSectionId::Connections)
                .unwrap()
                .default_subsection,
            Some(SettingsSubsectionId::Links)
        );
    }

    #[test]
    fn historical_link_and_media_slugs_keep_resolving() {
        for page in ["links-media", "connections", "uploader", "embeds"] {
            for (slug, anchor) in [
                ("links", SettingsSubsectionId::Links),
                ("link-previews", SettingsSubsectionId::LinkPreviews),
                ("image-uploads", SettingsSubsectionId::ImageUploads),
                ("players", SettingsSubsectionId::Players),
            ] {
                assert_eq!(
                    resolve_slug_path(page, Some(slug), false),
                    Some(SettingsLocation::anchor(
                        SettingsSectionId::Connections,
                        anchor
                    ))
                );
            }
        }
    }

    #[test]
    fn streamlink_search_keeps_its_visible_anchor_summary() {
        let model = SettingsSearchModel::compute("streamlink", false);
        let rows = super::super::settings_results::visible_settings_rows(
            &model,
            SettingsLocation::page(SettingsSectionId::Connections),
            None,
        );
        let players = rows
            .iter()
            .find(|row| {
                row.node
                    == super::super::settings_results::SettingsTreeNode::Anchor(
                        SettingsSectionId::Connections,
                        SettingsSubsectionId::Players,
                    )
            })
            .expect("Streamlink results must retain the Players anchor");
        assert_eq!(players.secondary.as_deref(), Some("Streamlink path · +3"));
        assert_eq!(players.badge, Some(4));
    }

    #[test]
    fn legacy_upload_keyword_reveal_opens_the_owning_editor() {
        let location = SettingsLocation::anchor(
            SettingsSectionId::Connections,
            SettingsSubsectionId::ImageUploads,
        );
        for query in ["headers", "request", "response mapping"] {
            assert_eq!(
                anchor_disclosures_for_query(location, query),
                &["uploads-custom-provider"]
            );
        }
        assert_eq!(
            anchor_disclosures_for_query(location, "upload history"),
            &["uploads-history"]
        );
        assert_eq!(
            anchor_disclosures_for_query(location, "drag"),
            &["uploads-more"]
        );
        assert_eq!(
            anchor_disclosures_for_query(location, ""),
            &["uploads-more"]
        );
    }

    #[test]
    fn moderation_migration_preserves_controls_and_editor_reveal_paths() {
        let targets: Vec<_> = setting_targets()
            .filter(|target| target.location.section == SettingsSectionId::Moderation)
            .collect();
        assert_eq!(targets.len(), 25);
        for (id, anchor, chain) in [
            (
                "moderation.show-pronouns",
                SettingsSubsectionId::UserCard,
                &[][..],
            ),
            (
                "moderation.button-command",
                SettingsSubsectionId::ModerationButtons,
                &["moderation-buttons-editor"][..],
            ),
            (
                "moderation.variables",
                SettingsSubsectionId::ModerationButtons,
                &["moderation-buttons-editor"][..],
            ),
            (
                "filters.expression",
                SettingsSubsectionId::FilterRules,
                &["filters-rules-editor"][..],
            ),
            (
                "filters.exclude-self",
                SettingsSubsectionId::FilterRules,
                &["filters-rules-editor"][..],
            ),
            (
                "filters.import-export",
                SettingsSubsectionId::FilterRules,
                &["filters-rules-editor"][..],
            ),
            (
                "filters.blocked-users-enabled",
                SettingsSubsectionId::BlockedUsers,
                &[][..],
            ),
            (
                "filters.blocked-users",
                SettingsSubsectionId::BlockedUsers,
                &["filters-blocked-users"][..],
            ),
            (
                "filters.similarity-enabled",
                SettingsSubsectionId::SimilarMessages,
                &[][..],
            ),
            (
                "filters.similarity-threshold",
                SettingsSubsectionId::SimilarMessages,
                &["filters-similarity-options"][..],
            ),
        ] {
            let target = setting_target_by_id(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(target.location.subsection, Some(anchor));
            assert_eq!(target.setting.disclosures, chain);
        }
    }

    #[test]
    fn historical_moderation_and_filter_anchors_keep_their_new_owner() {
        for (page, slug, anchor) in [
            (
                "moderation",
                "variables",
                SettingsSubsectionId::ModerationButtons,
            ),
            (
                "moderation-filters",
                "variables",
                SettingsSubsectionId::ModerationButtons,
            ),
            (
                "moderation",
                "moderation-buttons",
                SettingsSubsectionId::ModerationButtons,
            ),
            (
                "moderation-filters",
                "deleted-messages",
                SettingsSubsectionId::DeletedMessages,
            ),
            ("filters", "filter-rules", SettingsSubsectionId::FilterRules),
            (
                "filters",
                "blocked-users",
                SettingsSubsectionId::BlockedUsers,
            ),
            (
                "filters",
                "similar-messages",
                SettingsSubsectionId::SimilarMessages,
            ),
            (
                "moderation-filters",
                "similar-messages",
                SettingsSubsectionId::SimilarMessages,
            ),
        ] {
            assert_eq!(
                resolve_slug_path(page, Some(slug), false),
                Some(SettingsLocation::anchor(
                    SettingsSectionId::Moderation,
                    anchor
                ))
            );
        }
        assert_eq!(
            anchor_disclosures(SettingsLocation::anchor(
                SettingsSectionId::Moderation,
                SettingsSubsectionId::ModerationVariables
            )),
            &["moderation-buttons-editor"]
        );
    }

    #[test]
    fn highlights_migration_preserves_inventory_and_group_reveals() {
        let targets: Vec<_> = setting_targets()
            .filter(|target| target.location.section == SettingsSectionId::Highlights)
            .collect();
        assert_eq!(targets.len(), 88);
        for (id, anchor, chain) in [
            (
                "highlights.flash-on-mention",
                SettingsSubsectionId::HighlightsBehavior,
                &[][..],
            ),
            (
                "highlights.self-message-highlight",
                SettingsSubsectionId::HighlightsBehavior,
                &["highlights-behavior"][..],
            ),
            (
                "highlights.mention-color",
                SettingsSubsectionId::HighlightsPreviewColors,
                &["highlights-colors"][..],
            ),
            (
                "highlights.custom-rules",
                SettingsSubsectionId::HighlightsCustomRules,
                &["highlights-rules"][..],
            ),
            (
                "notifications.sound-event.mention.volume",
                SettingsSubsectionId::Sounds,
                &["sounds-events"][..],
            ),
            (
                "notifications.desktop",
                SettingsSubsectionId::DesktopNotifications,
                &[][..],
            ),
            (
                "notifications.live-status",
                SettingsSubsectionId::DesktopNotifications,
                &[][..],
            ),
            (
                "notifications.live-startup-delay",
                SettingsSubsectionId::DesktopNotifications,
                &["notifications-more"][..],
            ),
            (
                "notifications.muted-channels",
                SettingsSubsectionId::MutedChannels,
                &["notifications-muted"][..],
            ),
        ] {
            let target = setting_target_by_id(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(target.location.subsection, Some(anchor));
            assert_eq!(target.setting.disclosures, chain);
        }
        assert_eq!(
            section_descriptor(SettingsSectionId::Highlights)
                .unwrap()
                .default_subsection,
            Some(SettingsSubsectionId::HighlightsBehavior)
        );
    }

    #[test]
    fn historical_highlight_and_notification_slug_pairs_still_resolve() {
        for page in [
            "highlights-notifications",
            "highlights",
            "notifications",
            "sounds",
        ] {
            for (slug, expected, disclosure) in [
                (
                    "preview-colors",
                    SettingsSubsectionId::HighlightsPreviewColors,
                    Some("highlights-colors"),
                ),
                (
                    "behavior",
                    SettingsSubsectionId::HighlightsBehavior,
                    Some("highlights-behavior"),
                ),
                (
                    "custom-rules",
                    SettingsSubsectionId::HighlightsCustomRules,
                    Some("highlights-rules"),
                ),
                (
                    "sounds",
                    SettingsSubsectionId::Sounds,
                    Some("sounds-events"),
                ),
                (
                    "desktop-notifications",
                    SettingsSubsectionId::DesktopNotifications,
                    None,
                ),
                (
                    "live-channels",
                    SettingsSubsectionId::DesktopNotifications,
                    Some("notifications-more"),
                ),
                (
                    "muted-channels",
                    SettingsSubsectionId::MutedChannels,
                    Some("notifications-muted"),
                ),
            ] {
                let location = resolve_slug_path(page, Some(slug), false).unwrap();
                assert_eq!(
                    location,
                    SettingsLocation::anchor(SettingsSectionId::Highlights, expected)
                );
                if let Some(disclosure) = disclosure {
                    let raw = NOTIFICATIONS_SUBSECTIONS
                        .iter()
                        .chain(HIGHLIGHTS_SUBSECTIONS.iter())
                        .find(|anchor| anchor.slug == slug)
                        .unwrap();
                    assert_eq!(
                        anchor_disclosures(SettingsLocation::anchor(
                            SettingsSectionId::Highlights,
                            raw.id
                        )),
                        &[disclosure]
                    );
                }
            }
        }
    }

    #[test]
    fn flat_registry_has_eight_pages_and_thirty_eight_registered_anchors() {
        assert_eq!(SECTIONS.len(), 8);
        assert_eq!(
            SECTIONS
                .iter()
                .map(|section| section.subsections.len())
                .sum::<usize>(),
            38,
        );
        assert_eq!(CATEGORIES.len(), 1);
        assert_eq!(CATEGORIES[0].sections.len(), 8);
        let expected_setting_count = 366 + usize::from(cfg!(target_os = "windows")) * 2;
        assert_eq!(setting_targets().count(), expected_setting_count);
    }

    #[test]
    fn every_old_slug_and_anchor_pair_reaches_its_new_owner() {
        let legacy_pages = [
            (
                "appearance",
                SettingsSectionId::Theme,
                APPEARANCE_SUBSECTIONS,
            ),
            (
                "notifications",
                SettingsSectionId::Sounds,
                NOTIFICATIONS_SUBSECTIONS,
            ),
            ("updates", SettingsSectionId::Updates, UPDATES_SUBSECTIONS),
            ("chat", SettingsSectionId::ChatBehavior, CHAT_SUBSECTIONS),
            ("emotes", SettingsSectionId::Emotes, EMOTES_SUBSECTIONS),
            (
                "highlights",
                SettingsSectionId::Highlights,
                HIGHLIGHTS_SUBSECTIONS,
            ),
            ("filters", SettingsSectionId::Filters, FILTERS_SUBSECTIONS),
            (
                "commands",
                SettingsSectionId::Commands,
                COMMANDS_SUBSECTIONS,
            ),
            (
                "moderation",
                SettingsSectionId::Moderation,
                MODERATION_SUBSECTIONS,
            ),
            ("hotkeys", SettingsSectionId::Hotkeys, HOTKEYS_SUBSECTIONS),
            (
                "links-media",
                SettingsSectionId::Connections,
                LINKS_MEDIA_SUBSECTIONS,
            ),
            (
                "streamer-mode",
                SettingsSectionId::Privacy,
                STREAMER_MODE_SUBSECTIONS,
            ),
            (
                "developer-tools",
                SettingsSectionId::Advanced,
                DEVELOPER_TOOLS_SUBSECTIONS,
            ),
        ];
        for (slug, section, anchors) in legacy_pages {
            for anchor in anchors {
                let expected = canonical_location(SettingsLocation::anchor(section, anchor.id));
                for developer in [false, true] {
                    assert_eq!(
                        resolve_slug_path(slug, Some(anchor.slug), developer),
                        Some(if !developer && expected.is_developer_only() {
                            SettingsLocation::anchor(
                                SettingsSectionId::Advanced,
                                SettingsSubsectionId::DeveloperMode,
                            )
                        } else {
                            expected
                        }),
                        "{slug}/{}",
                        anchor.slug
                    );
                }
            }
        }
        assert_eq!(
            resolve_slug_path("account", None, false),
            Some(SettingsLocation::anchor(
                SettingsSectionId::General,
                SettingsSubsectionId::Account
            ))
        );
        assert_eq!(
            resolve_slug_path("hotkeys", None, false),
            Some(SettingsLocation::anchor(
                SettingsSectionId::Hotkeys,
                SettingsSubsectionId::HotkeysNavigation
            ))
        );
        assert_eq!(
            resolve_slug_path("nicknames", None, false),
            Some(SettingsLocation::anchor(
                SettingsSectionId::ChatBehavior,
                SettingsSubsectionId::ChatOptions
            ))
        );
    }

    #[test]
    fn ordinary_advanced_preferences_are_searchable_without_developer_mode() {
        let model = SettingsSearchModel::compute("Manual proxy URL", false);
        assert!(
            model
                .targets()
                .iter()
                .any(|target| target.row_id() == "developer-tools.proxy-url"
                    && !target.is_developer_only())
        );
        let model = SettingsSearchModel::compute("FPS", false);
        assert!(
            model
                .targets()
                .iter()
                .all(|target| !target.is_developer_only())
        );
        assert!(model.hidden_developer_targets().iter().all(|target| target.location.subsection == Some(SettingsSubsectionId::Diagnostics)));
    }

    #[test]
    fn old_navigation_preferences_keep_both_remembered_locations() {
        let prefs = SettingsNavigationPrefs {
            version: 1,
            last_standard: Some(PersistedSettingsLocation {
                section: "appearance".to_string(),
                subsection: Some("chat-display".to_string()),
            }),
            last_developer: Some(PersistedSettingsLocation {
                section: "developer-tools".to_string(),
                subsection: Some("network-proxy".to_string()),
            }),
            expanded_nodes: vec![
                "category:general".to_string(),
                "page:appearance".to_string(),
            ],
        };
        let standard = SettingsLocation::anchor(
            SettingsSectionId::ChatBehavior,
            SettingsSubsectionId::ChatReading,
        );
        assert_eq!(restore_location(&prefs, false), standard);
        assert_eq!(
            restore_developer_location(&prefs),
            SettingsLocation::anchor(
                SettingsSectionId::Advanced,
                SettingsSubsectionId::NetworkProxy
            )
        );
        assert_eq!(
            SettingsLocation::from_persisted(&standard.to_persisted(), false),
            Some(standard)
        );
    }

    #[test]
    fn hidden_diagnostics_persist_as_ordinary_advanced() {
        let diagnostics = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::Diagnostics,
        );
        let advanced = SettingsLocation::anchor(
            SettingsSectionId::Advanced,
            SettingsSubsectionId::DeveloperMode,
        );
        assert_eq!(persistence_location(diagnostics, false), advanced);
        assert_eq!(persistence_location(diagnostics, true), diagnostics);
    }
}
