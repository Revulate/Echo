//! UI action signals for cross-component communication
//!
//! These signals allow context menus and other components to trigger
//! actions that need to be handled by the main app (EchoApp).

use crate::backend::lock_utils::RwLockExt;
use crate::components::overlays::ContextMenuState;
use crate::state::SplitId;
use std::sync::LazyLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[path = "composer_send.rs"]
pub mod composer_send;

static NEXT_SEND_SUBMISSION: AtomicU64 = AtomicU64::new(1);

pub fn next_send_submission() -> u64 {
    NEXT_SEND_SUBMISSION.fetch_add(1, Ordering::Relaxed)
}

pub fn composer_account() -> Option<String> {
    crate::backend::settings::get_username().map(|login| login.trim().to_ascii_lowercase())
}

#[derive(Clone, Debug)]
pub struct ChatterListEntry {
    pub username: String,
    pub display_name: String,
    pub user_id: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum LinkOpenMode {
    Configured,
    Normal,
    Incognito,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationFilter {
    All,
    Automod,
}

#[derive(Clone, Debug)]
pub enum ComposerEditAction {
    Undo,
    Redo,
    Paste(String),
    /// Paste the clipboard as it is when the edit is applied. Menu actions
    /// have no `App`, and Linux reads the clipboard through GPUI.
    PasteClipboard,
    DeleteSelection {
        range: std::ops::Range<usize>,
        expected_text: String,
        copy: bool,
    },
    SelectAll,
}

#[derive(Clone, Debug)]
pub struct PendingComposerEdit {
    pub split_id: SplitId,
    pub action: ComposerEditAction,
}

/// UI action types that can be triggered from various components
#[derive(Clone, Debug)]
pub enum UiAction {
    /// Set reply state for a message
    Reply {
        channel: String,
        source_split_id: Option<SplitId>,
        message_id: String,
        sender: String,
        display_name: String,
        content: String,
    },
    /// View thread for a message
    ViewThread {
        message_id: String,
        channel: String,
        x: f32,
        y: f32,
    },
    /// Show context menu at position (delegated to OverlayManager)
    ShowContextMenu(ContextMenuState),
    /// Close the context menu and remove its escape-stack layer.
    CloseContextMenu,
    /// Show user card at position
    ShowUserCard {
        username: String,
        display_name: String,
        user_id: Option<String>,
        channel: String,
        color: Option<String>,
        badges: Vec<String>,
        x: f32,
        y: f32,
    },
    /// Open a standalone native channel chat popup window.
    OpenChannelPopup {
        channel: String,
    },
    /// Inspect a rich-card image in Echo's reusable native media window.
    ///
    /// This is intentionally separate from `ConfirmOpenLink`: inspecting the
    /// already-classified media asset must not open the card's browser target.
    OpenMediaInspector {
        source_url: String,
    },
    /// Open the chatter list immediately while the live fetch runs.
    OpenChatterListLoading {
        channel: String,
    },
    /// Reload the currently open chatter list using its active source.
    RefreshChatterList {
        channel: String,
        source: String,
    },
    /// Show a populated chatter list overlay.
    ShowChatterList {
        channel: String,
        chatters: Vec<ChatterListEntry>,
        total_count: usize,
        source: String,
        /// Rows are already normalized and ordered by Twitch login on the
        /// background command path. The UI can install identity indices for
        /// its default view instead of sorting the full result again.
        login_sorted: bool,
        /// More live Helix pages are still being fetched. The first page may
        /// populate the virtualized list while pagination continues.
        is_loading: bool,
    },
    /// Surface a chatter-list load failure without discarding the current rows.
    ChatterListLoadFailed {
        channel: String,
        source: String,
        error: String,
    },
    /// Remove a chatter-list entry after its remote moderation action succeeds.
    RemoveChatterListEntry {
        channel: String,
        source: String,
        username: String,
    },
    /// Insert @mention into the matching open channel input when available.
    InsertMention {
        username: String,
        channel: String,
        source_split_id: Option<SplitId>,
    },
    /// Insert copied message text into the matching channel composer without
    /// sending it. Message text is normalized into a safe non-command draft.
    InsertMessageTextIntoComposer {
        channel: String,
        /// The exact canvas split that opened the message menu, when available.
        /// EchoApp fences this against the current split/channel layout and never
        /// falls back to a different same-channel split when it is stale.
        source_split_id: Option<SplitId>,
        text: String,
        quote: bool,
    },
    /// Insert a completed image upload into its exact originating split draft.
    InsertUploadedLink {
        split_id: SplitId,
        channel: String,
        url: String,
    },
    /// Remove one retained mention from Activity Mentions.
    DismissMention {
        channel: String,
        message_id: String,
    },
    /// Prepare a whisper draft, preferring the matching open channel input when known.
    ComposeWhisper {
        username: String,
        channel: Option<String>,
    },
    /// Open or focus the virtual Whispers conversation split.
    OpenWhispersSplit,
    /// Mark one whisper thread as read without composing a reply.
    MarkWhisperRead {
        username: String,
    },
    /// Jump to a buffered chat message in the active tab.
    JumpToMessage {
        channel: String,
        message_id: String,
    },
    /// Jump to a buffered message in the exact split that owns an affordance.
    /// A stale split is deliberately not redirected to another same-channel
    /// leaf, because that would make a split-local control act elsewhere.
    JumpToMessageInSplit {
        split_id: SplitId,
        channel: String,
        message_id: String,
    },
    /// Ask the user before opening an external chat link.
    ConfirmOpenLink {
        url: String,
        mode: LinkOpenMode,
    },
    /// Open Activity Events, optionally narrowed to a class of events.
    OpenNotifications {
        filter: NotificationFilter,
    },
    /// Ask the user before removing a saved Twitch account.
    ConfirmRemoveAccount {
        username: String,
        display_name: String,
    },
    /// Ask the user before blocking a stable Twitch user identity.
    ConfirmBlockUser {
        user_id: String,
        username: String,
        display_name: String,
        expected_owner_user_id: String,
    },
    /// Ask the user before resetting Echo settings to defaults.
    ConfirmResetSettings,
    /// Ask before forgetting every locally retained Echo upload record.
    ConfirmClearUploadHistory,
    ConfirmUploaderMappingReplacement {
        saved: crate::log::Secret<crate::backend::image_uploader::ImageUploaderConfig>,
        replacement: crate::log::Secret<crate::backend::image_uploader::ImageUploaderConfig>,
        draft_generation: u64,
        origin: gpui::AnyWeakEntity,
        origin_window: gpui::AnyWindowHandle,
        status_revision: u64,
    },
    UploaderMappingConfirmationResolved {
        origin: gpui::AnyWeakEntity,
        origin_window: gpui::AnyWindowHandle,
        status_revision: u64,
        applied: bool,
    },
    CancelUploaderConfirmation {
        host: u64,
    },
    /// Ask before deleting Echo's readable channel log files.
    ConfirmClearReadableLogs,
    /// Ask before deleting Echo's history text exports.
    ConfirmClearHistoryExports,
    /// Ask before resetting every built-in hotkey to its default binding.
    ConfirmResetHotkeys,
    /// Ask before replacing the moderation buttons with the defaults.
    ConfirmResetModButtons,
    /// Ask before unmuting every channel's highlight sounds.
    ConfirmClearMutedSounds,
    /// Ask before deleting the persisted message history cache.
    ConfirmClearMessageCache,
    /// Ask before clearing the moderation log.
    ConfirmClearModLog,
    /// Ask before a Chatterino import replaces every open tab.
    ConfirmChatterinoTabReplace {
        tab_count: usize,
    },
    /// Ask before Close Other Tabs / Close Tabs to Right removes several tabs.
    ConfirmCloseTabs {
        tab_id: String,
        to_right: bool,
        count: usize,
    },
    /// Open the built-in Updates settings destination.
    OpenUpdatesSettings,
    /// Wake every open settings surface after the updater state changes.
    UpdateStateChanged,
    /// Re-render special Watching splits after its concrete browser channel changes.
    WatchingChannelChanged {
        channel: Option<String>,
    },
    /// Show a clickable update-available toast that routes to Updates.
    ShowUpdateAvailableToast {
        version: String,
    },
    /// Show a success toast notification through the overlay manager.
    ShowSuccessToast {
        message: String,
        detail: Option<String>,
    },
    /// Show a warning toast notification through the overlay manager.
    ShowWarningToast {
        message: String,
        detail: Option<String>,
    },
    /// Confirm an emote display rule change with an Undo action that applies
    /// `undo`.
    ShowEmoteDisplayRuleToast {
        message: String,
        undo: Vec<crate::backend::settings::EmoteRuleChange>,
    },
    /// Explain a chat row's highlight ("Why is this highlighted?").
    ExplainHighlight {
        highlight_type: crate::backend::HighlightType,
    },
    /// Open Highlights with the custom rule behind `highlight_type` loaded in
    /// the rule editor, or the rule list when that rule cannot be found.
    EditHighlightRule {
        highlight_type: crate::backend::HighlightType,
    },
}

/// Global queue for UI actions
pub static UI_ACTIONS: LazyLock<RwLock<Vec<UiAction>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Push one value into a queue, recovering the queue if a prior writer panicked.
fn enqueue<T>(queue: &RwLock<Vec<T>>, value: T) {
    queue.write_or_recover().push(value);
}

/// Push one value into a bounded queue, retaining its newest values.
fn enqueue_bounded<T>(queue: &RwLock<Vec<T>>, value: T, capacity: usize) {
    let mut values = queue.write_or_recover();
    if values.len() >= capacity {
        values.remove(0);
    }
    values.push(value);
}

/// Drain every queued value, recovering the queue if a prior writer panicked.
fn drain<T>(queue: &RwLock<Vec<T>>) -> Vec<T> {
    std::mem::take(&mut *queue.write_or_recover())
}

/// Push an action to the queue
pub fn push_action(action: UiAction) {
    enqueue(&UI_ACTIONS, action);
    crate::managers::frame_rate_controller::record_activity();
}

/// Pop all pending actions (drains the queue)
pub fn pop_actions() -> Vec<UiAction> {
    drain(&UI_ACTIONS)
}

/// Pending mention insertion data
#[derive(Clone, Debug)]
pub struct PendingMentionInsert {
    pub username: String,
    pub channel: String,
    pub source_split_id: Option<SplitId>,
}

/// Pending uploaded-link insertion tied to one stable originating split.
#[derive(Clone, Debug)]
pub struct PendingUploadedLinkInsert {
    pub split_id: SplitId,
    pub channel: String,
    pub url: String,
}

pub struct PendingConfirmedFileUpload {
    pub split_id: SplitId,
    pub channel: String,
    pub account: Option<String>,
    pub paths: Vec<std::path::PathBuf>,
    pub source: crate::backend::image_uploader::ImageFileSource,
}

static PENDING_CONFIRMED_FILE_UPLOADS: LazyLock<RwLock<Vec<PendingConfirmedFileUpload>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

pub fn push_confirmed_file_upload(upload: PendingConfirmedFileUpload) {
    enqueue(&PENDING_CONFIRMED_FILE_UPLOADS, upload);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_confirmed_file_uploads() -> Vec<PendingConfirmedFileUpload> {
    drain(&PENDING_CONFIRMED_FILE_UPLOADS)
}

/// The composer draft behind a send, handed back to its split if Twitch or the
/// network rejects the message.
#[derive(Clone, Debug)]
pub struct SendDraftOrigin {
    pub submission_id: u64,
    pub account: Option<String>,
    pub split_id: SplitId,
    pub text: String,
    pub reply: Option<crate::state::ReplyState>,
}

#[derive(Clone, Debug)]
pub struct PendingCompletedSend {
    pub origin: SendDraftOrigin,
    pub channel: String,
    pub notice: Option<String>,
}

/// A rejected composer send waiting for EchoApp to restore or point at history.
#[derive(Clone, Debug)]
pub struct PendingFailedSend {
    pub origin: SendDraftOrigin,
    pub channel: String,
    pub action: &'static str,
    pub error: String,
    /// Twitch may have posted the message anyway, so the text must not go
    /// back into the draft.
    pub delivery_unknown: bool,
}

/// Bounded, normalized message text awaiting insertion into its originating
/// split composer when known, or a matching channel composer otherwise.
#[derive(Clone, Debug)]
pub struct PendingMessageComposerInsert {
    pub channel: String,
    pub source_split_id: Option<SplitId>,
    pub text: String,
}

/// Keep context-menu insertions small enough to be predictable for the native
/// composer while preserving normal Unicode chat text.
pub const MAX_MESSAGE_COMPOSER_INSERT_CHARS: usize = 500;

/// Pending reply activation data
#[derive(Clone, Debug)]
pub struct PendingReply {
    pub channel: String,
    pub source_split_id: Option<SplitId>,
    pub message_id: String,
    pub sender: String,
    pub display_name: String,
    pub content: String,
}

/// Pending thread viewer open request
#[derive(Clone, Debug)]
pub struct PendingThreadView {
    pub message_id: String,
    pub channel: String,
    pub x: f32,
    pub y: f32,
}

/// Pending search-result jump request.
#[derive(Clone, Debug)]
pub struct PendingSearchJump {
    pub channel: String,
    pub message_id: String,
}

/// Pending message jump that is fenced to one stable split identity.
#[derive(Clone, Debug)]
pub struct PendingSplitMessageJump {
    pub split_id: SplitId,
    pub channel: String,
    pub message_id: String,
}

/// Pending whisper compose request.
#[derive(Clone, Debug)]
pub struct PendingWhisperCompose {
    pub username: String,
    pub channel: Option<String>,
}

/// Pending quick-switch request.
#[derive(Clone, Debug)]
pub struct PendingQuickSwitch {
    pub tab_index: usize,
    pub split_path: Vec<usize>,
}

/// Global queue for pending mention insertions
static PENDING_MENTIONS: LazyLock<RwLock<Vec<PendingMentionInsert>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Global queue for uploaded links that require EchoApp-owned input access.
static PENDING_UPLOADED_LINKS: LazyLock<RwLock<Vec<PendingUploadedLinkInsert>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Global queue for safe message-text composer insertions.
static PENDING_MESSAGE_COMPOSER_INSERTS: LazyLock<RwLock<Vec<PendingMessageComposerInsert>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
const MAX_PENDING_MESSAGE_COMPOSER_INSERTS: usize = 32;
/// Global queue for pending replies
static PENDING_REPLIES: LazyLock<RwLock<Vec<PendingReply>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Global queue for pending thread viewer opens
static PENDING_THREAD_VIEWS: LazyLock<RwLock<Vec<PendingThreadView>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Global queue for pending search-result jumps
static PENDING_SEARCH_JUMPS: LazyLock<RwLock<Vec<PendingSearchJump>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
static PENDING_SPLIT_MESSAGE_JUMPS: LazyLock<RwLock<Vec<PendingSplitMessageJump>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
const MAX_PENDING_SPLIT_MESSAGE_JUMPS: usize = 32;
/// Global queue for pending whisper compose requests.
static PENDING_WHISPER_COMPOSES: LazyLock<RwLock<Vec<PendingWhisperCompose>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
static PENDING_OPEN_WHISPERS_SPLIT: AtomicBool = AtomicBool::new(false);
static PENDING_OPEN_LOGIN: AtomicBool = AtomicBool::new(false);
static PENDING_REFOCUS_ACTIVE_INPUT: AtomicBool = AtomicBool::new(false);
/// Request a new tab from surfaces that do not own EchoApp directly (such as
/// the retained tab-bar entity).
static PENDING_OPEN_NEW_TAB: AtomicUsize = AtomicUsize::new(0);
/// Global queue for pending quick-switch requests.
static PENDING_QUICK_SWITCHES: LazyLock<RwLock<Vec<PendingQuickSwitch>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Followed live channels picked in the quick switcher.
static PENDING_FOLLOWED_CHANNEL_OPENS: LazyLock<RwLock<Vec<PendingFollowedChannelOpen>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// A followed live channel to open: in `fill_split` while that split is still
/// empty, otherwise in a new tab (or where it is already open).
#[derive(Clone, Debug)]
pub struct PendingFollowedChannelOpen {
    pub login: String,
    pub fill_split: Option<SplitId>,
}
static PENDING_COMPOSER_EDITS: LazyLock<RwLock<Vec<PendingComposerEdit>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
static PENDING_REOPEN_CLOSED_TAB: AtomicUsize = AtomicUsize::new(0);
static PENDING_BULK_TAB_CLOSES: LazyLock<RwLock<Vec<PendingBulkTabClose>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// A confirmed Close Other Tabs / Close Tabs to Right request.
#[derive(Clone, Debug)]
pub struct PendingBulkTabClose {
    pub tab_id: String,
    pub to_right: bool,
}

pub fn push_pending_reopen_closed_tab() {
    let _ = PENDING_REOPEN_CLOSED_TAB.try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        Some(count.saturating_add(1).min(10))
    });
    crate::managers::frame_rate_controller::record_activity();
}

pub fn take_pending_reopen_closed_tab() -> bool {
    PENDING_REOPEN_CLOSED_TAB
        .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            count.checked_sub(1)
        })
        .is_ok()
}

pub fn push_pending_bulk_tab_close(tab_id: String, to_right: bool) {
    enqueue_bounded(
        &PENDING_BULK_TAB_CLOSES,
        PendingBulkTabClose { tab_id, to_right },
        8,
    );
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_pending_bulk_tab_closes() -> Vec<PendingBulkTabClose> {
    drain(&PENDING_BULK_TAB_CLOSES)
}

static PENDING_FAILED_SENDS: LazyLock<RwLock<Vec<PendingFailedSend>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
static PENDING_COMPLETED_SENDS: LazyLock<RwLock<Vec<PendingCompletedSend>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

pub fn push_pending_completed_send(completed: PendingCompletedSend) {
    enqueue(&PENDING_COMPLETED_SENDS, completed);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_pending_completed_sends() -> Vec<PendingCompletedSend> {
    drain(&PENDING_COMPLETED_SENDS)
}

pub fn push_pending_failed_send(failed: PendingFailedSend) {
    enqueue(&PENDING_FAILED_SENDS, failed);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_pending_failed_sends() -> Vec<PendingFailedSend> {
    drain(&PENDING_FAILED_SENDS)
}

pub fn push_pending_composer_edit(split_id: SplitId, action: ComposerEditAction) {
    enqueue(
        &PENDING_COMPOSER_EDITS,
        PendingComposerEdit { split_id, action },
    );
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_pending_composer_edits() -> Vec<PendingComposerEdit> {
    drain(&PENDING_COMPOSER_EDITS)
}

/// Push a pending mention insertion (called from PollingCoordinator)
pub fn push_pending_mention(username: String, channel: String, source_split_id: Option<SplitId>) {
    enqueue(
        &PENDING_MENTIONS,
        PendingMentionInsert {
            username,
            channel,
            source_split_id,
        },
    );
}

/// Drain all pending mention insertions (called from EchoApp render loop)
pub fn drain_pending_mentions() -> Vec<PendingMentionInsert> {
    drain(&PENDING_MENTIONS)
}

/// Push a completed upload link for exact split-local insertion.
pub fn push_pending_uploaded_link(split_id: SplitId, channel: String, url: String) {
    enqueue(
        &PENDING_UPLOADED_LINKS,
        PendingUploadedLinkInsert {
            split_id,
            channel,
            url,
        },
    );
}

/// Drain completed upload links on the GPUI thread.
pub fn drain_pending_uploaded_links() -> Vec<PendingUploadedLinkInsert> {
    drain(&PENDING_UPLOADED_LINKS)
}

/// Normalize copied chat text for a composer insertion. Newlines and control
/// characters collapse to one space; quote insertions always begin with `> `.
/// Unquoted copied commands receive a visible `\\` prefix, so neither action can
/// turn a copied leading slash into a command when the user later sends a draft.
fn normalize_message_composer_insert(text: &str, quote: bool) -> Option<String> {
    let prefix = if quote { "> " } else { "" };
    let max_body_chars = MAX_MESSAGE_COMPOSER_INSERT_CHARS.saturating_sub(prefix.chars().count());
    let mut normalized = String::with_capacity(text.len().min(max_body_chars));
    let mut pending_space = false;
    let mut normalized_chars = 0;

    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_control() {
            pending_space |= !normalized.is_empty();
            continue;
        }
        let added_chars = usize::from(pending_space) + 1;
        if normalized_chars + added_chars > max_body_chars {
            break;
        }
        if pending_space {
            normalized.push(' ');
            normalized_chars += 1;
            pending_space = false;
        }
        normalized.push(ch);
        normalized_chars += 1;
    }

    (!normalized.is_empty()).then(|| {
        if quote {
            format!("{prefix}{normalized}")
        } else if normalized.starts_with('/') {
            if normalized_chars == MAX_MESSAGE_COMPOSER_INSERT_CHARS {
                normalized.pop();
            }
            format!("\\{normalized}")
        } else {
            normalized
        }
    })
}

/// Queue a non-destructive insertion for the main app, which resolves the
/// originating split when known or a matching open channel split otherwise
/// before touching the draft. Empty/unsafe-normalized payloads are
/// intentionally discarded and never sent.
pub fn push_pending_message_composer_insert(
    channel: String,
    source_split_id: Option<SplitId>,
    text: String,
    quote: bool,
) {
    let Some(text) = normalize_message_composer_insert(&text, quote) else {
        return;
    };
    enqueue_bounded(
        &PENDING_MESSAGE_COMPOSER_INSERTS,
        PendingMessageComposerInsert {
            channel,
            source_split_id,
            text,
        },
        MAX_PENDING_MESSAGE_COMPOSER_INSERTS,
    );
    crate::managers::frame_rate_controller::record_activity();
}

/// Drain message-text composer insertions on the GPUI thread.
pub fn drain_pending_message_composer_inserts() -> Vec<PendingMessageComposerInsert> {
    drain(&PENDING_MESSAGE_COMPOSER_INSERTS)
}

/// Push a pending reply activation (called from PollingCoordinator)
pub fn push_pending_reply(reply: PendingReply) {
    enqueue(&PENDING_REPLIES, reply);
}

/// Drain all pending replies (called from EchoApp render loop)
pub fn drain_pending_replies() -> Vec<PendingReply> {
    drain(&PENDING_REPLIES)
}

/// Push a pending thread view request (called from PollingCoordinator)
pub fn push_pending_thread_view(view: PendingThreadView) {
    enqueue(&PENDING_THREAD_VIEWS, view);
}

/// Drain all pending thread view requests (called from EchoApp render loop)
pub fn drain_pending_thread_views() -> Vec<PendingThreadView> {
    drain(&PENDING_THREAD_VIEWS)
}

/// Push a pending search-result jump.
pub fn push_pending_search_jump(channel: String, message_id: String) {
    enqueue(
        &PENDING_SEARCH_JUMPS,
        PendingSearchJump {
            channel,
            message_id,
        },
    );
}

/// Drain all pending search-result jumps.
pub fn drain_pending_search_jumps() -> Vec<PendingSearchJump> {
    drain(&PENDING_SEARCH_JUMPS)
}

/// Queue a message jump that must remain within its source split.
pub fn push_pending_split_message_jump(split_id: SplitId, channel: String, message_id: String) {
    enqueue_bounded(
        &PENDING_SPLIT_MESSAGE_JUMPS,
        PendingSplitMessageJump {
            split_id,
            channel,
            message_id,
        },
        MAX_PENDING_SPLIT_MESSAGE_JUMPS,
    );
}

/// Drain split-fenced message jumps.
pub fn drain_pending_split_message_jumps() -> Vec<PendingSplitMessageJump> {
    drain(&PENDING_SPLIT_MESSAGE_JUMPS)
}

/// Push a pending whisper compose request.
pub fn push_pending_whisper_compose(username: String, channel: Option<String>) {
    enqueue(
        &PENDING_WHISPER_COMPOSES,
        PendingWhisperCompose { username, channel },
    );
}

/// Drain all pending whisper compose requests.
pub fn drain_pending_whisper_composes() -> Vec<PendingWhisperCompose> {
    drain(&PENDING_WHISPER_COMPOSES)
}

pub fn push_pending_open_new_tab() {
    let _ = PENDING_OPEN_NEW_TAB.try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        Some(count.saturating_add(1).min(32))
    });
    crate::managers::frame_rate_controller::record_activity();
}

pub fn take_pending_open_new_tab() -> bool {
    PENDING_OPEN_NEW_TAB
        .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            count.checked_sub(1)
        })
        .is_ok()
}

pub fn push_pending_open_whispers_split() {
    PENDING_OPEN_WHISPERS_SPLIT.store(true, Ordering::Release);
}

pub fn take_pending_open_whispers_split() -> bool {
    PENDING_OPEN_WHISPERS_SPLIT.swap(false, Ordering::AcqRel)
}

/// Ask the app to focus the active split's message box, for overlays that close
/// themselves and have no handle on the app.
pub fn push_pending_refocus_active_input() {
    PENDING_REFOCUS_ACTIVE_INPUT.store(true, Ordering::Release);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn take_pending_refocus_active_input() -> bool {
    PENDING_REFOCUS_ACTIVE_INPUT.swap(false, Ordering::AcqRel)
}

pub fn push_pending_open_login() {
    PENDING_OPEN_LOGIN.store(true, Ordering::Release);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn take_pending_open_login() -> bool {
    PENDING_OPEN_LOGIN.swap(false, Ordering::AcqRel)
}

/// Push a pending quick-switch request.
pub fn push_pending_quick_switch(tab_index: usize, split_path: Vec<usize>) {
    enqueue(
        &PENDING_QUICK_SWITCHES,
        PendingQuickSwitch {
            tab_index,
            split_path,
        },
    );
}

/// Drain all pending quick-switch requests.
pub fn drain_pending_quick_switches() -> Vec<PendingQuickSwitch> {
    drain(&PENDING_QUICK_SWITCHES)
}

/// Ask the app to open a followed live channel and focus it.
pub fn push_pending_followed_channel_open(login: String, fill_split: Option<SplitId>) {
    enqueue_bounded(
        &PENDING_FOLLOWED_CHANNEL_OPENS,
        PendingFollowedChannelOpen { login, fill_split },
        8,
    );
    crate::managers::frame_rate_controller::record_activity();
}

pub fn drain_pending_followed_channel_opens() -> Vec<PendingFollowedChannelOpen> {
    drain(&PENDING_FOLLOWED_CHANNEL_OPENS)
}

#[cfg(test)]
mod tests {
    use super::{drain, enqueue, enqueue_bounded};
    use core::prelude::v1::test;
    use std::sync::{Arc, RwLock};

    fn poison(queue: Arc<RwLock<Vec<u8>>>) {
        let result = std::thread::spawn(move || {
            let _guard = queue.write().expect("test queue lock should be available");
            panic!("deliberately poison the queue");
        })
        .join();
        assert!(result.is_err());
    }

    #[test]
    fn enqueue_and_drain_recover_a_poisoned_queue() {
        let queue = Arc::new(RwLock::new(vec![1]));
        poison(Arc::clone(&queue));

        enqueue(&queue, 2);

        assert_eq!(drain(&queue), vec![1, 2]);
    }

    #[test]
    fn bounded_enqueue_recovers_a_poisoned_queue_and_keeps_the_newest_values() {
        let queue = Arc::new(RwLock::new(vec![0]));
        poison(Arc::clone(&queue));

        enqueue_bounded(&queue, 1, 2);
        enqueue_bounded(&queue, 2, 2);
        enqueue_bounded(&queue, 3, 2);

        assert_eq!(drain(&queue), vec![2, 3]);
    }
}
