//! Hotkey model: rebindable actions, scopes, bindings, and defaults.
//!
//! Handles key binding storage, matching, and defaults.
//! Supports rebindable actions with configurable modifiers.
//!
//! Keystroke normalization is the application's job; this module matches an
//! already-normalized `(key, ctrl, shift, alt)` tuple so the model stays free
//! of the UI toolkit's event types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const CUSTOM_HOTKEY_PREFIX: &str = "customHotkey:";

// ============================================================================
// Hotkey Actions
// ============================================================================

/// All rebindable hotkey actions in Echo
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyAction {
    // Navigation
    ToggleSettings,
    ToggleAccountSwitcher,
    ToggleNotifications,
    ToggleSearch,
    ToggleGlobalSearch,
    ToggleWhispers,
    ToggleMentions,
    NewTab,
    NewSplit,
    NextTab,
    PrevTab,
    CloseSplit,
    CloseTab,
    MoveTabLeft,
    MoveTabRight,
    OpenTabMenu,
    RenameTab,
    SelectTab1,
    SelectTab2,
    SelectTab3,
    SelectTab4,
    SelectTab5,
    SelectTab6,
    SelectTab7,
    SelectTab8,
    SelectTab9,
    PopupActiveSplit,
    PopupActiveTab,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    ToggleTabBarVisibility,
    ToggleLiveTabsOnly,
    ToggleSidebar,
    // Chat
    ChangeChannel,
    FocusInput,
    SendMessage,
    SendMessageKeepInput,
    OpenEmotePicker,
    ClearChat,
    CopyMessage,
    CreateClip,
    OpenChannelInBrowser,
    OpenChannelInStreamlink,
    OpenChannelInCustomPlayer,
    OpenModViewInBrowser,
    OpenViewerList,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ReconnectActiveChannel,
    EnableHighlightSounds,
    DisableHighlightSounds,
    ToggleHighlightSounds,
    OpenSubscriptionPage,
    FocusSplitLeft,
    FocusSplitRight,
    FocusSplitUp,
    FocusSplitDown,
    // Search
    SearchPreviousResult,
    SearchNextResult,
    SearchOpenSelectedResult,
    SearchPreviousPage,
    SearchNextPage,
    SearchFirstResult,
    SearchLastResult,
    SearchClearQuery,
    // Moderation
    ToggleModMode,
    ToggleModLog,
    QuickTimeout30s,
    QuickTimeout10m,
    QuickBan,
    // Other
    ClearMentions,
    MarkWhispersRead,
    CloseThreadViewer,
    ReloadEmotes,
    ToggleFullscreen,
    OpenDebugPopup,
    ToggleCheatSheet,
    ToggleMetrics,
    ToggleStreamerOverlay,
    ReopenClosedSplit,
    ReopenClosedTab,
    ToggleLocalR9K,
    QuickSwitcher,
    ToggleTransparency,
    ToggleAlwaysOnTop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyScope {
    Global,
    ActiveSplit,
    SearchModal,
    MentionsSidebar,
    WhispersSidebar,
    ThreadViewer,
    UserCard,
}

impl HotkeyScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::ActiveSplit => "Active split",
            Self::SearchModal => "Search modal",
            Self::MentionsSidebar => "Activity: Mentions",
            Self::WhispersSidebar => "Activity: Whispers",
            Self::ThreadViewer => "Thread viewer",
            Self::UserCard => "User card",
        }
    }

    pub fn priority(&self) -> u8 {
        match self {
            Self::SearchModal => 6,
            Self::UserCard => 5,
            Self::ThreadViewer => 4,
            Self::MentionsSidebar | Self::WhispersSidebar => 3,
            Self::ActiveSplit => 2,
            Self::Global => 1,
        }
    }

    pub fn is_available(&self, context: &HotkeyMatchContext) -> bool {
        match self {
            Self::Global => context.allow_global,
            Self::ActiveSplit => context.has_active_split,
            Self::SearchModal => context.has_search_modal,
            Self::MentionsSidebar => context.has_mentions_sidebar,
            Self::WhispersSidebar => context.has_whispers_sidebar,
            Self::ThreadViewer => context.has_thread_viewer,
            Self::UserCard => context.has_user_card,
        }
    }

    pub fn conflicts_with(&self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Global, _)
                | (_, Self::Global)
                | (Self::ActiveSplit, Self::ActiveSplit)
                | (Self::SearchModal, Self::SearchModal)
                | (Self::MentionsSidebar, Self::MentionsSidebar)
                | (Self::MentionsSidebar, Self::WhispersSidebar)
                | (Self::WhispersSidebar, Self::MentionsSidebar)
                | (Self::WhispersSidebar, Self::WhispersSidebar)
                | (Self::ThreadViewer, Self::ThreadViewer)
                | (Self::UserCard, Self::UserCard)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomHotkeyAction {
    pub id: String,
    pub name: String,
    pub command: String,
}

impl CustomHotkeyAction {
    pub fn scope(&self) -> HotkeyScope {
        HotkeyScope::ActiveSplit
    }

    pub fn scope_label(&self) -> &'static str {
        self.scope().label()
    }

    pub fn description(&self) -> String {
        format!("Send `{}` through the active split.", self.command)
    }

    pub fn storage_key(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        format!("{CUSTOM_HOTKEY_PREFIX}{json}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchedHotkey {
    Builtin(HotkeyAction),
    CustomCommand(CustomHotkeyAction),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HotkeyMatchContext {
    pub allow_global: bool,
    pub has_active_split: bool,
    pub has_search_modal: bool,
    pub has_mentions_sidebar: bool,
    pub has_whispers_sidebar: bool,
    pub has_thread_viewer: bool,
    pub has_user_card: bool,
}

/// Every built-in action, in declaration order.
pub const ALL_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::ToggleSettings,
    HotkeyAction::ToggleAccountSwitcher,
    HotkeyAction::ToggleNotifications,
    HotkeyAction::ToggleSearch,
    HotkeyAction::ToggleGlobalSearch,
    HotkeyAction::ToggleWhispers,
    HotkeyAction::ToggleMentions,
    HotkeyAction::NewTab,
    HotkeyAction::NewSplit,
    HotkeyAction::NextTab,
    HotkeyAction::PrevTab,
    HotkeyAction::CloseSplit,
    HotkeyAction::CloseTab,
    HotkeyAction::MoveTabLeft,
    HotkeyAction::MoveTabRight,
    HotkeyAction::OpenTabMenu,
    HotkeyAction::RenameTab,
    HotkeyAction::SelectTab1,
    HotkeyAction::SelectTab2,
    HotkeyAction::SelectTab3,
    HotkeyAction::SelectTab4,
    HotkeyAction::SelectTab5,
    HotkeyAction::SelectTab6,
    HotkeyAction::SelectTab7,
    HotkeyAction::SelectTab8,
    HotkeyAction::SelectTab9,
    HotkeyAction::PopupActiveSplit,
    HotkeyAction::PopupActiveTab,
    HotkeyAction::ZoomIn,
    HotkeyAction::ZoomOut,
    HotkeyAction::ResetZoom,
    HotkeyAction::ToggleTabBarVisibility,
    HotkeyAction::ToggleLiveTabsOnly,
    HotkeyAction::ToggleSidebar,
    HotkeyAction::ChangeChannel,
    HotkeyAction::FocusInput,
    HotkeyAction::SendMessage,
    HotkeyAction::SendMessageKeepInput,
    HotkeyAction::OpenEmotePicker,
    HotkeyAction::ClearChat,
    HotkeyAction::CopyMessage,
    HotkeyAction::CreateClip,
    HotkeyAction::OpenChannelInBrowser,
    HotkeyAction::OpenChannelInStreamlink,
    HotkeyAction::OpenChannelInCustomPlayer,
    HotkeyAction::OpenModViewInBrowser,
    HotkeyAction::OpenViewerList,
    HotkeyAction::ScrollUp,
    HotkeyAction::ScrollDown,
    HotkeyAction::ScrollToTop,
    HotkeyAction::ScrollToBottom,
    HotkeyAction::ReconnectActiveChannel,
    HotkeyAction::EnableHighlightSounds,
    HotkeyAction::DisableHighlightSounds,
    HotkeyAction::ToggleHighlightSounds,
    HotkeyAction::OpenSubscriptionPage,
    HotkeyAction::FocusSplitLeft,
    HotkeyAction::FocusSplitRight,
    HotkeyAction::FocusSplitUp,
    HotkeyAction::FocusSplitDown,
    HotkeyAction::SearchPreviousResult,
    HotkeyAction::SearchNextResult,
    HotkeyAction::SearchOpenSelectedResult,
    HotkeyAction::SearchPreviousPage,
    HotkeyAction::SearchNextPage,
    HotkeyAction::SearchFirstResult,
    HotkeyAction::SearchLastResult,
    HotkeyAction::SearchClearQuery,
    HotkeyAction::ToggleModMode,
    HotkeyAction::ToggleModLog,
    HotkeyAction::QuickTimeout30s,
    HotkeyAction::QuickTimeout10m,
    HotkeyAction::QuickBan,
    HotkeyAction::ClearMentions,
    HotkeyAction::MarkWhispersRead,
    HotkeyAction::CloseThreadViewer,
    HotkeyAction::ReloadEmotes,
    HotkeyAction::ToggleFullscreen,
    HotkeyAction::OpenDebugPopup,
    HotkeyAction::ToggleCheatSheet,
    HotkeyAction::ToggleMetrics,
    HotkeyAction::ToggleStreamerOverlay,
    HotkeyAction::ReopenClosedSplit,
    HotkeyAction::ReopenClosedTab,
    HotkeyAction::ToggleLocalR9K,
    HotkeyAction::QuickSwitcher,
    HotkeyAction::ToggleTransparency,
    HotkeyAction::ToggleAlwaysOnTop,
];

pub const NAVIGATION_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::ToggleSettings,
    HotkeyAction::ToggleAccountSwitcher,
    HotkeyAction::ToggleNotifications,
    HotkeyAction::ToggleSearch,
    HotkeyAction::ToggleGlobalSearch,
    HotkeyAction::ToggleWhispers,
    HotkeyAction::ToggleMentions,
    HotkeyAction::NewTab,
    HotkeyAction::NewSplit,
    HotkeyAction::NextTab,
    HotkeyAction::PrevTab,
    HotkeyAction::CloseSplit,
    HotkeyAction::CloseTab,
    HotkeyAction::MoveTabLeft,
    HotkeyAction::MoveTabRight,
    HotkeyAction::OpenTabMenu,
    HotkeyAction::RenameTab,
    HotkeyAction::SelectTab1,
    HotkeyAction::SelectTab2,
    HotkeyAction::SelectTab3,
    HotkeyAction::SelectTab4,
    HotkeyAction::SelectTab5,
    HotkeyAction::SelectTab6,
    HotkeyAction::SelectTab7,
    HotkeyAction::SelectTab8,
    HotkeyAction::SelectTab9,
    HotkeyAction::PopupActiveSplit,
    HotkeyAction::PopupActiveTab,
    HotkeyAction::ZoomIn,
    HotkeyAction::ZoomOut,
    HotkeyAction::ResetZoom,
    HotkeyAction::ToggleTabBarVisibility,
    HotkeyAction::ToggleLiveTabsOnly,
    HotkeyAction::ToggleSidebar,
];

pub const CHAT_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::ChangeChannel,
    HotkeyAction::FocusInput,
    HotkeyAction::SendMessage,
    HotkeyAction::SendMessageKeepInput,
    HotkeyAction::OpenEmotePicker,
    HotkeyAction::ClearChat,
    HotkeyAction::CopyMessage,
    HotkeyAction::CreateClip,
    HotkeyAction::OpenChannelInBrowser,
    HotkeyAction::OpenChannelInStreamlink,
    HotkeyAction::OpenChannelInCustomPlayer,
    HotkeyAction::OpenModViewInBrowser,
    HotkeyAction::OpenViewerList,
    HotkeyAction::ScrollUp,
    HotkeyAction::ScrollDown,
    HotkeyAction::ScrollToTop,
    HotkeyAction::ScrollToBottom,
    HotkeyAction::ReconnectActiveChannel,
    HotkeyAction::EnableHighlightSounds,
    HotkeyAction::DisableHighlightSounds,
    HotkeyAction::ToggleHighlightSounds,
    HotkeyAction::OpenSubscriptionPage,
    HotkeyAction::FocusSplitLeft,
    HotkeyAction::FocusSplitRight,
    HotkeyAction::FocusSplitUp,
    HotkeyAction::FocusSplitDown,
];

pub const SEARCH_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::SearchPreviousResult,
    HotkeyAction::SearchNextResult,
    HotkeyAction::SearchOpenSelectedResult,
    HotkeyAction::SearchPreviousPage,
    HotkeyAction::SearchNextPage,
    HotkeyAction::SearchFirstResult,
    HotkeyAction::SearchLastResult,
    HotkeyAction::SearchClearQuery,
];

pub const MODERATION_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::ToggleModMode,
    HotkeyAction::ToggleModLog,
    HotkeyAction::QuickTimeout30s,
    HotkeyAction::QuickTimeout10m,
    HotkeyAction::QuickBan,
];

pub const OTHER_ACTIONS: &[HotkeyAction] = &[
    HotkeyAction::ClearMentions,
    HotkeyAction::MarkWhispersRead,
    HotkeyAction::CloseThreadViewer,
    HotkeyAction::ReloadEmotes,
    HotkeyAction::ToggleFullscreen,
    HotkeyAction::OpenDebugPopup,
    HotkeyAction::ToggleCheatSheet,
    HotkeyAction::ToggleMetrics,
    HotkeyAction::ToggleStreamerOverlay,
    HotkeyAction::ReopenClosedSplit,
    HotkeyAction::ReopenClosedTab,
    HotkeyAction::ToggleLocalR9K,
    HotkeyAction::QuickSwitcher,
    HotkeyAction::ToggleTransparency,
    HotkeyAction::ToggleAlwaysOnTop,
];

impl HotkeyAction {
    /// Get human-readable display name
    pub fn display_name(&self) -> &'static str {
        use HotkeyAction::*;
        match self {
            ToggleSettings => "Open settings",
            ToggleAccountSwitcher => "Toggle account switcher",
            ToggleNotifications => "Toggle notifications",
            ToggleSearch => "Open search",
            ToggleGlobalSearch => "Open global search",
            ToggleWhispers => "Toggle whispers",
            ToggleMentions => "Toggle mentions",
            NewTab => "New tab",
            NewSplit => "New split",
            NextTab => "Next tab",
            PrevTab => "Previous tab",
            CloseSplit => "Close split",
            CloseTab => "Close tab",
            MoveTabLeft => "Move tab left",
            MoveTabRight => "Move tab right",
            OpenTabMenu => "Open tab menu",
            RenameTab => "Rename tab",
            SelectTab1 => "Select tab 1",
            SelectTab2 => "Select tab 2",
            SelectTab3 => "Select tab 3",
            SelectTab4 => "Select tab 4",
            SelectTab5 => "Select tab 5",
            SelectTab6 => "Select tab 6",
            SelectTab7 => "Select tab 7",
            SelectTab8 => "Select tab 8",
            SelectTab9 => "Select last tab",
            PopupActiveSplit => "Popup active split",
            PopupActiveTab => "Popup active tab",
            ZoomIn => "Zoom in",
            ZoomOut => "Zoom out",
            ResetZoom => "Reset zoom",
            ToggleTabBarVisibility => "Toggle tab bar visibility",
            ToggleLiveTabsOnly => "Toggle live tabs only",
            ToggleSidebar => "Toggle sidebar",
            ChangeChannel => "Change channel",
            FocusInput => "Focus input",
            SendMessage => "Send message",
            SendMessageKeepInput => "Send message and keep input",
            OpenEmotePicker => "Open emote picker",
            ClearChat => "Clear chat",
            CopyMessage => "Copy message",
            CreateClip => "Create clip",
            OpenChannelInBrowser => "Open channel in browser",
            OpenChannelInStreamlink => "Open channel in Streamlink",
            OpenChannelInCustomPlayer => "Open channel in custom player",
            OpenModViewInBrowser => "Open moderator view in browser",
            OpenViewerList => "Open viewer list",
            ScrollUp => "Scroll up",
            ScrollDown => "Scroll down",
            ScrollToTop => "Scroll to top",
            ScrollToBottom => "Scroll to bottom",
            ReconnectActiveChannel => "Reconnect active channel",
            EnableHighlightSounds => "Unmute channel notifications",
            DisableHighlightSounds => "Mute channel notifications",
            ToggleHighlightSounds => "Toggle channel notifications",
            OpenSubscriptionPage => "Open subscription page",
            FocusSplitLeft => "Focus split left",
            FocusSplitRight => "Focus split right",
            FocusSplitUp => "Focus split up",
            FocusSplitDown => "Focus split down",
            SearchPreviousResult => "Previous search result",
            SearchNextResult => "Next search result",
            SearchOpenSelectedResult => "Open search result",
            SearchPreviousPage => "Previous search page",
            SearchNextPage => "Next search page",
            SearchFirstResult => "First search result",
            SearchLastResult => "Last search result",
            SearchClearQuery => "Clear search query",
            ToggleModMode => "Toggle mod mode",
            ToggleModLog => "Toggle mod log",
            QuickTimeout30s => "Quick timeout (30s)",
            QuickTimeout10m => "Quick timeout (10m)",
            QuickBan => "Quick ban",
            ClearMentions => "Clear mentions",
            MarkWhispersRead => "Mark whispers read",
            CloseThreadViewer => "Close thread viewer",
            ReloadEmotes => "Reload emotes",
            ToggleFullscreen => "Toggle fullscreen",
            OpenDebugPopup => "Toggle debug popup",
            ToggleCheatSheet => "Keyboard shortcuts",
            ToggleMetrics => "Toggle metrics",
            ToggleStreamerOverlay => "Toggle overlay window",
            ReopenClosedSplit => "Reopen closed split",
            ReopenClosedTab => "Reopen closed tab",
            ToggleLocalR9K => "Toggle local R9K",
            QuickSwitcher => "Quick channel switcher",
            ToggleTransparency => "Toggle window transparency",
            ToggleAlwaysOnTop => "Toggle always on top",
        }
    }

    /// User-visible scope where the action can run.
    pub fn scope(&self) -> HotkeyScope {
        use HotkeyAction::*;
        match self {
            FocusInput
            | ChangeChannel
            | SendMessage
            | SendMessageKeepInput
            | OpenEmotePicker
            | ClearChat
            | CopyMessage
            | CreateClip
            | OpenChannelInBrowser
            | OpenChannelInStreamlink
            | OpenChannelInCustomPlayer
            | OpenModViewInBrowser
            | OpenViewerList
            | ScrollUp
            | ScrollDown
            | ScrollToTop
            | ScrollToBottom
            | ReconnectActiveChannel
            | EnableHighlightSounds
            | DisableHighlightSounds
            | ToggleHighlightSounds
            | OpenSubscriptionPage
            | FocusSplitLeft
            | FocusSplitRight
            | FocusSplitUp
            | FocusSplitDown
            | ReloadEmotes
            | ToggleModMode
            | OpenDebugPopup
            | ToggleStreamerOverlay => HotkeyScope::ActiveSplit,
            SearchPreviousResult
            | SearchNextResult
            | SearchOpenSelectedResult
            | SearchPreviousPage
            | SearchNextPage
            | SearchFirstResult
            | SearchLastResult
            | SearchClearQuery => HotkeyScope::SearchModal,
            ClearMentions => HotkeyScope::MentionsSidebar,
            MarkWhispersRead => HotkeyScope::WhispersSidebar,
            CloseThreadViewer => HotkeyScope::ThreadViewer,
            QuickTimeout30s | QuickTimeout10m | QuickBan => HotkeyScope::UserCard,
            _ => HotkeyScope::Global,
        }
    }

    /// User-visible scope where the action can run.
    pub fn scope_label(&self) -> &'static str {
        self.scope().label()
    }

    /// Short user-facing action detail for settings and shortcut reference UI.
    pub fn description(&self) -> &'static str {
        use HotkeyAction::*;
        match self {
            ToggleSettings => "Open or focus Echo settings.",
            ToggleAccountSwitcher => "Open or close the account switcher.",
            ToggleNotifications => "Open or close Activity at Events.",
            ToggleSearch => "Open message search for the active tab.",
            ToggleGlobalSearch => "Open message search across every open tab.",
            ToggleWhispers => "Open or close Activity at Whispers.",
            ToggleMentions => "Open or close Activity at Mentions.",
            NewTab => "Create and focus a new empty tab.",
            NewSplit => "Create a new empty split next to the active split.",
            NextTab => "Move focus to the next tab.",
            PrevTab => "Move focus to the previous tab.",
            CloseSplit => "Close the active split.",
            CloseTab => "Close the active tab.",
            MoveTabLeft => "Move the active tab one position to the left.",
            MoveTabRight => "Move the active tab one position to the right.",
            OpenTabMenu => "Open actions for the active tab.",
            RenameTab => "Rename the active tab.",
            SelectTab1 => "Switch to the first visible tab.",
            SelectTab2 => "Switch to the second visible tab.",
            SelectTab3 => "Switch to the third visible tab.",
            SelectTab4 => "Switch to the fourth visible tab.",
            SelectTab5 => "Switch to the fifth visible tab.",
            SelectTab6 => "Switch to the sixth visible tab.",
            SelectTab7 => "Switch to the seventh visible tab.",
            SelectTab8 => "Switch to the eighth visible tab.",
            SelectTab9 => "Switch to the last visible tab.",
            PopupActiveSplit => "Open the active split in a standalone chat window.",
            PopupActiveTab => "Open the active tab's channels in standalone chat windows.",
            ZoomIn => "Increase the global chat text scale.",
            ZoomOut => "Decrease the global chat text scale.",
            ResetZoom => "Reset the global chat text scale to 100%.",
            ToggleTabBarVisibility => "Toggle the tab bar between hidden and show-all modes.",
            ToggleLiveTabsOnly => "Toggle a tab-bar filter that only shows live tabs.",
            ToggleSidebar => "Open or close the Activity rail.",
            ChangeChannel => "Turn the active split into an editable channel prompt.",
            FocusInput => "Focus the active chat input.",
            SendMessage => "Send the active input text.",
            SendMessageKeepInput => "Send the active input text without clearing it.",
            OpenEmotePicker => "Open the emote picker for the active split.",
            ClearChat => "Clear the active channel buffer.",
            CopyMessage => "Copy the current chat selection.",
            CreateClip => "Create a Twitch clip from the active channel.",
            OpenChannelInBrowser => {
                "Open the active channel's Twitch page in the configured browser."
            }
            OpenChannelInStreamlink => "Open the active channel with configured Streamlink.",
            OpenChannelInCustomPlayer => "Open the active channel with the configured player.",
            OpenModViewInBrowser => {
                "Open the active channel's Twitch moderator view in the configured browser."
            }
            OpenViewerList => "Open the active channel's chatter list.",
            ScrollUp => "Scroll the active split upward.",
            ScrollDown => "Scroll the active split downward.",
            ScrollToTop => "Jump the active split to oldest buffered messages.",
            ScrollToBottom => "Jump the active split to newest messages.",
            ReconnectActiveChannel => "Reconnect the active channel.",
            EnableHighlightSounds => {
                "Allow sounds and desktop notifications for the active channel."
            }
            DisableHighlightSounds => {
                "Mute sounds and desktop notifications for the active channel."
            }
            ToggleHighlightSounds => {
                "Mute or unmute sounds and desktop notifications for the active channel."
            }
            OpenSubscriptionPage => "Open the active channel's Twitch subscription page.",
            FocusSplitLeft => "Move focus to the nearest split on the left.",
            FocusSplitRight => "Move focus to the nearest split on the right.",
            FocusSplitUp => "Move focus to the nearest split above.",
            FocusSplitDown => "Move focus to the nearest split below.",
            SearchPreviousResult => "Move search selection to the previous result.",
            SearchNextResult => "Move search selection to the next result.",
            SearchOpenSelectedResult => "Open the selected search result or run the search.",
            SearchPreviousPage => "Move search selection one results page upward.",
            SearchNextPage => "Move search selection one results page downward.",
            SearchFirstResult => "Jump search selection to the first result.",
            SearchLastResult => "Jump search selection to the last result.",
            SearchClearQuery => "Clear the current search query and results.",
            ToggleModMode => "Toggle moderation controls for the active split.",
            ToggleModLog => "Open or close the moderation action log.",
            QuickTimeout30s => "Timeout the open user-card target for 30 seconds.",
            QuickTimeout10m => "Timeout the open user-card target for 10 minutes.",
            QuickBan => "Ban the open user-card target.",
            ClearMentions => "Clear retained entries from Activity Mentions.",
            MarkWhispersRead => "Mark every whisper thread as read.",
            CloseThreadViewer => "Close the open thread viewer overlay.",
            ReloadEmotes => "Refresh emotes for the active channel.",
            ToggleFullscreen => "Toggle the main window fullscreen state.",
            OpenDebugPopup => "Open or close the standalone debug popup for the active split.",
            ToggleCheatSheet => "Open or close the shortcut reference.",
            ToggleMetrics => "Show or hide runtime metrics.",
            ToggleStreamerOverlay => "Open or close an overlay window for the active split.",
            ReopenClosedSplit => "Reopen the most recently closed split.",
            ReopenClosedTab => "Reopen the most recently closed tab or tabs.",
            ToggleLocalR9K => "Toggle local hiding of similar messages.",
            QuickSwitcher => "Open quick channel and tab switching.",
            ToggleTransparency => "Toggle transparent window mode.",
            ToggleAlwaysOnTop => "Toggle always-on-top window mode.",
        }
    }

    /// The persisted settings key: the variant name in camelCase, exactly as
    /// serde writes this enum.
    pub fn as_str(&self) -> &'static str {
        use HotkeyAction::*;
        match self {
            ToggleSettings => "toggleSettings",
            ToggleAccountSwitcher => "toggleAccountSwitcher",
            ToggleNotifications => "toggleNotifications",
            ToggleSearch => "toggleSearch",
            ToggleGlobalSearch => "toggleGlobalSearch",
            ToggleWhispers => "toggleWhispers",
            ToggleMentions => "toggleMentions",
            NewTab => "newTab",
            NewSplit => "newSplit",
            NextTab => "nextTab",
            PrevTab => "prevTab",
            CloseSplit => "closeSplit",
            CloseTab => "closeTab",
            MoveTabLeft => "moveTabLeft",
            MoveTabRight => "moveTabRight",
            OpenTabMenu => "openTabMenu",
            RenameTab => "renameTab",
            SelectTab1 => "selectTab1",
            SelectTab2 => "selectTab2",
            SelectTab3 => "selectTab3",
            SelectTab4 => "selectTab4",
            SelectTab5 => "selectTab5",
            SelectTab6 => "selectTab6",
            SelectTab7 => "selectTab7",
            SelectTab8 => "selectTab8",
            SelectTab9 => "selectTab9",
            PopupActiveSplit => "popupActiveSplit",
            PopupActiveTab => "popupActiveTab",
            ZoomIn => "zoomIn",
            ZoomOut => "zoomOut",
            ResetZoom => "resetZoom",
            ToggleTabBarVisibility => "toggleTabBarVisibility",
            ToggleLiveTabsOnly => "toggleLiveTabsOnly",
            ToggleSidebar => "toggleSidebar",
            ChangeChannel => "changeChannel",
            FocusInput => "focusInput",
            SendMessage => "sendMessage",
            SendMessageKeepInput => "sendMessageKeepInput",
            OpenEmotePicker => "openEmotePicker",
            ClearChat => "clearChat",
            CopyMessage => "copyMessage",
            CreateClip => "createClip",
            OpenChannelInBrowser => "openChannelInBrowser",
            OpenChannelInStreamlink => "openChannelInStreamlink",
            OpenChannelInCustomPlayer => "openChannelInCustomPlayer",
            OpenModViewInBrowser => "openModViewInBrowser",
            OpenViewerList => "openViewerList",
            ScrollUp => "scrollUp",
            ScrollDown => "scrollDown",
            ScrollToTop => "scrollToTop",
            ScrollToBottom => "scrollToBottom",
            ReconnectActiveChannel => "reconnectActiveChannel",
            EnableHighlightSounds => "enableHighlightSounds",
            DisableHighlightSounds => "disableHighlightSounds",
            ToggleHighlightSounds => "toggleHighlightSounds",
            OpenSubscriptionPage => "openSubscriptionPage",
            FocusSplitLeft => "focusSplitLeft",
            FocusSplitRight => "focusSplitRight",
            FocusSplitUp => "focusSplitUp",
            FocusSplitDown => "focusSplitDown",
            SearchPreviousResult => "searchPreviousResult",
            SearchNextResult => "searchNextResult",
            SearchOpenSelectedResult => "searchOpenSelectedResult",
            SearchPreviousPage => "searchPreviousPage",
            SearchNextPage => "searchNextPage",
            SearchFirstResult => "searchFirstResult",
            SearchLastResult => "searchLastResult",
            SearchClearQuery => "searchClearQuery",
            ToggleModMode => "toggleModMode",
            ToggleModLog => "toggleModLog",
            QuickTimeout30s => "quickTimeout30s",
            QuickTimeout10m => "quickTimeout10m",
            QuickBan => "quickBan",
            ClearMentions => "clearMentions",
            MarkWhispersRead => "markWhispersRead",
            CloseThreadViewer => "closeThreadViewer",
            ReloadEmotes => "reloadEmotes",
            ToggleFullscreen => "toggleFullscreen",
            OpenDebugPopup => "openDebugPopup",
            ToggleCheatSheet => "toggleCheatSheet",
            ToggleMetrics => "toggleMetrics",
            ToggleStreamerOverlay => "toggleStreamerOverlay",
            ReopenClosedSplit => "reopenClosedSplit",
            ReopenClosedTab => "reopenClosedTab",
            ToggleLocalR9K => "toggleLocalR9K",
            QuickSwitcher => "quickSwitcher",
            ToggleTransparency => "toggleTransparency",
            ToggleAlwaysOnTop => "toggleAlwaysOnTop",
        }
    }
}

impl std::fmt::Display for HotkeyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A settings key that names no built-in hotkey action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownHotkeyAction;

impl std::str::FromStr for HotkeyAction {
    type Err = UnknownHotkeyAction;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        ALL_ACTIONS
            .iter()
            .copied()
            .find(|action| action.as_str() == key)
            .ok_or(UnknownHotkeyAction)
    }
}

// ============================================================================
// Hotkey Binding
// ============================================================================

/// A single key binding with modifiers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HotkeyBinding {
    /// The main key (e.g., "tab", "f", "1", "escape")
    pub key: String,
    /// Ctrl/Cmd modifier
    pub ctrl: bool,
    /// Shift modifier
    pub shift: bool,
    /// Alt modifier
    pub alt: bool,
}

impl HotkeyBinding {
    /// Create a new binding
    pub fn new(key: impl Into<String>, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            key: key.into().to_lowercase(),
            ctrl,
            shift,
            alt,
        }
    }

    /// Convert to display string (e.g., "Ctrl+Shift+Tab")
    pub fn to_display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }

        // Format key nicely
        let key_display = match self.key.as_str() {
            "," => ",".to_string(),
            "tab" => "Tab".to_string(),
            "enter" => "Enter".to_string(),
            "escape" => "Esc".to_string(),
            "backspace" => "Backspace".to_string(),
            "up" => "Up".to_string(),
            "down" => "Down".to_string(),
            "left" => "Left".to_string(),
            "right" => "Right".to_string(),
            "pageup" => "Page Up".to_string(),
            "pagedown" => "Page Down".to_string(),
            "end" => "End".to_string(),
            "home" => "Home".to_string(),
            "f5" => "F5".to_string(),
            "f11" => "F11".to_string(),
            "/" => "/".to_string(),
            k => k.to_uppercase(),
        };
        parts.push(&key_display);

        parts.join("+")
    }

    pub fn conflict_key(&self) -> Option<String> {
        let key = self.key.trim().to_lowercase();
        if key.is_empty() {
            return None;
        }

        Some(format!("{}:{}:{}:{}", self.ctrl, self.shift, self.alt, key))
    }

    /// Check if this binding matches an already-normalized keystroke
    pub fn matches_keystroke(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        self.key == key && self.ctrl == ctrl && self.shift == shift && self.alt == alt
    }
}

// ============================================================================
// Default Bindings
// ============================================================================

/// Get the default bindings, keyed by [`HotkeyAction::as_str`].
///
/// Actions listed with `UNBOUND` have an empty binding: they appear in the
/// settings UI as unassigned. Actions missing from the table have no entry.
pub fn default_hotkeys() -> HashMap<String, HotkeyBinding> {
    use HotkeyAction::*;
    /// `(ctrl, shift, alt)`
    type Mods = (bool, bool, bool);
    const NONE: Mods = (false, false, false);
    const CTRL: Mods = (true, false, false);
    const SHIFT: Mods = (false, true, false);
    const ALT: Mods = (false, false, true);
    const CTRL_SHIFT: Mods = (true, true, false);
    const CTRL_ALT: Mods = (true, false, true);
    const CTRL_SHIFT_ALT: Mods = (true, true, true);
    const UNBOUND: &str = "";
    const DEFAULTS: &[(HotkeyAction, &str, Mods)] = &[
        // Navigation
        (ToggleSettings, "p", CTRL),
        (ToggleAccountSwitcher, "a", CTRL_ALT),
        (ToggleNotifications, UNBOUND, NONE),
        (ToggleSearch, "f", CTRL),
        (ToggleGlobalSearch, "f", CTRL_SHIFT),
        // Ctrl+Shift+W closes the tab, so Whispers has no default.
        (ToggleWhispers, UNBOUND, NONE),
        (ToggleMentions, "m", CTRL_SHIFT),
        (NewTab, "t", CTRL_SHIFT),
        (NewSplit, "t", CTRL),
        (NextTab, "tab", CTRL),
        (PrevTab, "tab", CTRL_SHIFT),
        // Ctrl+W closes the split and Ctrl+Shift+W closes the tab.
        (CloseSplit, "w", CTRL),
        (CloseTab, "w", CTRL_SHIFT),
        (MoveTabLeft, "pageup", CTRL_SHIFT),
        (MoveTabRight, "pagedown", CTRL_SHIFT),
        (OpenTabMenu, "f10", SHIFT),
        (RenameTab, "f2", NONE),
        (SelectTab1, "1", CTRL),
        (SelectTab2, "2", CTRL),
        (SelectTab3, "3", CTRL),
        (SelectTab4, "4", CTRL),
        (SelectTab5, "5", CTRL),
        (SelectTab6, "6", CTRL),
        (SelectTab7, "7", CTRL),
        (SelectTab8, "8", CTRL),
        (SelectTab9, "9", CTRL),
        (PopupActiveSplit, "n", CTRL),
        (PopupActiveTab, "n", CTRL_SHIFT),
        (ZoomIn, "=", CTRL),
        (ZoomOut, "-", CTRL),
        (ResetZoom, "0", CTRL),
        (ToggleTabBarVisibility, "l", CTRL_SHIFT_ALT),
        (ToggleLiveTabsOnly, "l", CTRL_SHIFT),
        (ToggleSidebar, "b", CTRL),
        // Chat
        (ChangeChannel, "r", CTRL),
        (FocusInput, UNBOUND, NONE),
        (SendMessage, "enter", NONE),
        (SendMessageKeepInput, "enter", CTRL),
        (OpenEmotePicker, "e", CTRL),
        (ClearChat, "l", CTRL),
        (CopyMessage, "c", CTRL),
        (CreateClip, "x", ALT),
        (OpenChannelInBrowser, UNBOUND, NONE),
        (OpenChannelInStreamlink, UNBOUND, NONE),
        (OpenChannelInCustomPlayer, UNBOUND, NONE),
        (OpenModViewInBrowser, UNBOUND, NONE),
        (OpenViewerList, UNBOUND, NONE),
        (ScrollUp, "pageup", NONE),
        (ScrollDown, "pagedown", NONE),
        (ScrollToTop, "home", CTRL),
        (ScrollToBottom, "end", CTRL),
        (ReconnectActiveChannel, "f5", CTRL),
        (EnableHighlightSounds, UNBOUND, NONE),
        (DisableHighlightSounds, UNBOUND, NONE),
        (ToggleHighlightSounds, UNBOUND, NONE),
        (OpenSubscriptionPage, UNBOUND, NONE),
        (FocusSplitLeft, "left", ALT),
        (FocusSplitRight, "right", ALT),
        (FocusSplitUp, "up", ALT),
        (FocusSplitDown, "down", ALT),
        // Search
        (SearchPreviousResult, "up", NONE),
        (SearchNextResult, "down", NONE),
        (SearchOpenSelectedResult, "enter", NONE),
        (SearchPreviousPage, "pageup", NONE),
        (SearchNextPage, "pagedown", NONE),
        (SearchFirstResult, "home", NONE),
        (SearchLastResult, "end", NONE),
        (SearchClearQuery, "u", CTRL),
        // Moderation
        (ToggleModMode, "m", CTRL),
        (ToggleModLog, UNBOUND, NONE),
        (QuickTimeout30s, UNBOUND, NONE),
        (QuickTimeout10m, UNBOUND, NONE),
        (QuickBan, UNBOUND, NONE),
        // Other
        (ReloadEmotes, "f5", NONE),
        (ToggleFullscreen, "f11", NONE),
        (OpenDebugPopup, "f10", NONE),
        (ToggleCheatSheet, "/", CTRL),
        (ToggleMetrics, "p", CTRL_SHIFT),
        (ToggleStreamerOverlay, "n", CTRL_ALT),
        (ReopenClosedSplit, "g", CTRL),
        (ReopenClosedTab, "g", CTRL_SHIFT),
        (ToggleLocalR9K, "h", CTRL),
        (QuickSwitcher, "k", CTRL),
        (ToggleTransparency, UNBOUND, NONE),
        (ToggleAlwaysOnTop, "a", CTRL_SHIFT),
    ];

    DEFAULTS
        .iter()
        .map(|&(action, key, (ctrl, shift, alt))| {
            (
                action.to_string(),
                HotkeyBinding::new(key, ctrl, shift, alt),
            )
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_serialization() {
        let action = HotkeyAction::ToggleSettings;
        let s = action.to_string();
        assert_eq!(s, "toggleSettings");
        assert_eq!(s.parse(), Ok(action));
    }

    #[test]
    fn action_keys_match_serde_names() {
        let categories: usize = [
            NAVIGATION_ACTIONS,
            CHAT_ACTIONS,
            SEARCH_ACTIONS,
            MODERATION_ACTIONS,
            OTHER_ACTIONS,
        ]
        .iter()
        .map(|actions| actions.len())
        .sum();
        assert_eq!(ALL_ACTIONS.len(), categories);
        for action in ALL_ACTIONS {
            let serde_name = serde_json::to_string(action).unwrap();
            assert_eq!(serde_name.trim_matches('"'), action.as_str());
            assert_eq!(action.as_str().parse(), Ok(*action));
        }
    }

    #[test]
    fn test_binding_display() {
        let binding = HotkeyBinding::new("tab", true, true, false);
        assert_eq!(binding.to_display(), "Ctrl+Shift+Tab");

        let simple = HotkeyBinding::new("f11", false, false, false);
        assert_eq!(simple.to_display(), "F11");
    }

    #[test]
    fn test_default_hotkeys_count() {
        let defaults = default_hotkeys();
        assert_eq!(defaults.len(), 85);
    }

    #[test]
    fn default_hotkeys_have_no_scope_conflicts() {
        let defaults: Vec<(HotkeyAction, String)> = default_hotkeys()
            .into_iter()
            .filter_map(|(name, binding)| Some((name.parse().ok()?, binding.conflict_key()?)))
            .collect();
        for (index, (action, key)) in defaults.iter().enumerate() {
            for (other, other_key) in &defaults[index + 1..] {
                assert!(
                    key != other_key || !action.scope().conflicts_with(other.scope()),
                    "{action:?} and {other:?} share default {key}"
                );
            }
        }
    }
}
