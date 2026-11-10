//! Account-scoped Twitch blocked-user cache and ingestion policy.
//!
//! Twitch supplies the authoritative list through Helix. Echo keeps only IDs,
//! logins, and display names in memory, generation-fences account changes, and
//! checks the cache before expensive message parsing/layout work.

use crate::backend::auth::normalize_oauth_token;
use crate::backend::constants::CLIENT_ID;
use crate::backend::lock_utils::RwLockExt;
use crate::backend::settings::BlockedUserVisibility;
use arc_swap::ArcSwap;
use parking_lot::{Mutex, MutexGuard};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;
use url::Url;

const PAGE_SIZE: usize = 100;
/// The settings surface intentionally exposes a bounded identity list, while
/// ingestion keeps compact membership for the complete Helix cursor stream.
const MAX_DISPLAY_USERS: usize = 1_000;
const MAX_REMOTE_MEMBERSHIP: usize = 50_000;
const MAX_LOCAL_MUTATIONS: usize = 256;
const MAX_CACHED_MEMBERSHIP: usize = MAX_REMOTE_MEMBERSHIP + MAX_LOCAL_MUTATIONS;
const MAX_BLOCKED_PAGES: usize = MAX_REMOTE_MEMBERSHIP / PAGE_SIZE;
const MAX_PAGE_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_PENDING_CHANNEL_ROWS: usize = 8_192;
const MAX_PENDING_WHISPERS: usize = 128;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(45);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MUTATION_ERROR_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_MANUAL_AUTH_RECOVERY_ATTEMPTS: usize = 3;

static HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(crate::backend::http::client));
static AUTH_EPOCH: AtomicU64 = AtomicU64::new(0);
static REFRESH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Message admission reads these on every Twitch row. They mirror the current
/// visibility transaction without consulting SETTINGS, OAuth strings, or the
/// mutable cache.
static INITIAL_VISIBILITY_PENDING: AtomicBool = AtomicBool::new(false);
static RESIDENT_VISIBILITY_REVOKED: AtomicBool = AtomicBool::new(false);
static PENDING_CHANNEL_OVERFLOW_WARNED: AtomicBool = AtomicBool::new(false);
static CHAT_STATE_RECONCILE_PENDING: AtomicBool = AtomicBool::new(false);
static CHAT_STATE_RECONCILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static VISIBILITY_COMMIT_GATE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static PENDING_VISIBILITY: LazyLock<Mutex<PendingVisibility>> =
    LazyLock::new(|| Mutex::new(PendingVisibility::default()));
static CHAT_STATE_RECONCILE_CHANNELS: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CACHE: LazyLock<RwLock<BlockedUserCache>> =
    LazyLock::new(|| RwLock::new(BlockedUserCache::default()));
/// Immutable membership published independently of CACHE so live message
/// admission and bulk reconciliation never contend on the mutable cache lock.
static PUBLISHED_MEMBERSHIP: LazyLock<ArcSwap<BlockedMembership>> =
    LazyLock::new(|| ArcSwap::from_pointee(BlockedMembership::default()));

/// Holding the visibility commit gate. Functions named `*_commit_held` take a
/// `&VisibilityCommit` so they can only run inside that critical section.
pub(crate) struct VisibilityCommit {
    _gate: MutexGuard<'static, ()>,
}

/// Serialize membership publication and the final externally-visible commit of
/// chat/history/whisper producers. Callers must acquire this before CACHE and
/// must never hold it across network or parsing work.
pub(crate) fn visibility_commit_guard() -> VisibilityCommit {
    VisibilityCommit {
        _gate: VISIBILITY_COMMIT_GATE.lock(),
    }
}

#[derive(Clone)]
pub(crate) struct PendingChannelRow {
    pub channel: String,
    pub message_id: String,
    auth_epoch: u64,
}

#[derive(Clone)]
pub(crate) struct PendingWhisper {
    pub sender_login: String,
    pub message: crate::backend::models::ChatMessage,
    sender_user_id: String,
    auth_epoch: u64,
    owner_user_id: Option<String>,
}

#[derive(Default)]
struct PendingVisibility {
    channel_rows: VecDeque<PendingChannelRow>,
    whispers: VecDeque<PendingWhisper>,
}

#[derive(Default)]
pub(crate) struct PendingVisibilityRelease {
    pub channel_rows: Vec<PendingChannelRow>,
    pub whispers: Vec<PendingWhisper>,
    /// A full account transition temporarily hid every resident row, so all
    /// channel snapshots must be refreshed when this transaction terminates.
    pub reveal_resident_rows: bool,
}

/// Register a backend-only row while the first block-list baseline is pending.
/// The caller owns the visibility commit gate. A full queue rejects the newest
/// row in O(1); callers must check capacity before mutating the chat buffer.
pub(crate) fn defer_channel_row_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    channel: &str,
    message_id: &str,
) -> bool {
    let mut pending = PENDING_VISIBILITY.lock();
    if pending.channel_rows.len() >= MAX_PENDING_CHANNEL_ROWS {
        return false;
    }
    pending.channel_rows.push_back(PendingChannelRow {
        channel: channel.to_string(),
        message_id: message_id.to_string(),
        auth_epoch: AUTH_EPOCH.load(Ordering::Acquire),
    });
    true
}

/// Remaining globally bounded row capacity. The caller owns the visibility
/// commit gate, so admission followed by registration cannot race a producer.
pub(crate) fn pending_channel_capacity_remaining_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) -> usize {
    MAX_PENDING_CHANNEL_ROWS.saturating_sub(PENDING_VISIBILITY.lock().channel_rows.len())
}

/// Log the first rejection in a transaction without spamming high-volume chat.
pub(crate) fn should_log_pending_channel_overflow() -> bool {
    !PENDING_CHANNEL_OVERFLOW_WARNED.swap(true, Ordering::AcqRel)
}

/// Retain one incoming whisper without exposing its sender/content until the
/// current account's initial block-list attempt becomes authoritative or
/// deliberately fails open. The queue is FIFO and explicitly bounded.
pub(crate) fn defer_whisper_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    sender_user_id: &str,
    sender_login: &str,
    message: crate::backend::models::ChatMessage,
) -> bool {
    let owner_user_id = CACHE
        .read_or_recover()
        .active_session
        .as_ref()
        .map(|session| session.owner_user_id.clone());
    let mut pending = PENDING_VISIBILITY.lock();
    let dropped_oldest = if pending.whispers.len() >= MAX_PENDING_WHISPERS {
        pending.whispers.pop_front();
        true
    } else {
        false
    };
    pending.whispers.push_back(PendingWhisper {
        sender_login: sender_login.to_string(),
        message,
        sender_user_id: sender_user_id.to_string(),
        auth_epoch: AUTH_EPOCH.load(Ordering::Acquire),
        owner_user_id,
    });
    dropped_oldest
}

/// Take only pending work owned by the terminal authentication epoch. Blocked
/// whispers are discarded before their content leaves this module.
pub(crate) fn take_pending_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    auth_epoch: u64,
) -> PendingVisibilityRelease {
    if AUTH_EPOCH.load(Ordering::Acquire) != auth_epoch {
        return PendingVisibilityRelease::default();
    }
    let active_owner = CACHE
        .read_or_recover()
        .active_session
        .as_ref()
        .map(|session| session.owner_user_id.clone());
    let (channel_rows, whispers) = {
        let mut pending = PENDING_VISIBILITY.lock();
        PENDING_CHANNEL_OVERFLOW_WARNED.store(false, Ordering::Release);
        (
            pending.channel_rows.drain(..).collect::<Vec<_>>(),
            pending.whispers.drain(..).collect::<Vec<_>>(),
        )
    };
    INITIAL_VISIBILITY_PENDING.store(false, Ordering::Release);
    let reveal_resident_rows = RESIDENT_VISIBILITY_REVOKED.load(Ordering::Acquire);

    PendingVisibilityRelease {
        channel_rows: channel_rows
            .into_iter()
            .filter(|row| row.auth_epoch == auth_epoch)
            .collect(),
        whispers: whispers
            .into_iter()
            .filter(|whisper| {
                whisper.auth_epoch == auth_epoch
                    && whisper.owner_user_id.is_some()
                    && whisper.owner_user_id.as_ref() == active_owner.as_ref()
                    && !should_hide_whisper(Some(&whisper.sender_user_id), &whisper.sender_login)
            })
            .collect(),
        reveal_resident_rows,
    }
}

/// The caller has armed the per-channel ChatState handoff while still holding
/// the visibility gate, so retained UI snapshots can no longer observe a gap.
pub(crate) fn release_resident_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) {
    RESIDENT_VISIBILITY_REVOKED.store(false, Ordering::Release);
}

/// Snapshot the backend-only row IDs for one channel. The caller owns the
/// visibility commit gate, making this coherent with terminal prune/release.
pub(crate) fn pending_channel_row_ids_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    channel: &str,
) -> HashSet<String> {
    let auth_epoch = AUTH_EPOCH.load(Ordering::Acquire);
    PENDING_VISIBILITY
        .lock()
        .channel_rows
        .iter()
        .filter(|row| row.auth_epoch == auth_epoch && row.channel.eq_ignore_ascii_case(channel))
        .map(|row| row.message_id.clone())
        .collect()
}

fn clear_pending_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) -> Vec<PendingChannelRow> {
    let mut pending = PENDING_VISIBILITY.lock();
    pending.whispers.clear();
    PENDING_CHANNEL_OVERFLOW_WARNED.store(false, Ordering::Release);
    pending.channel_rows.drain(..).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedUser {
    pub user_id: String,
    pub user_login: String,
    pub display_name: String,
}

/// One bounded page of identities for the Settings surface.
///
/// Ingestion uses the complete compact membership indexes; Settings only
/// clones the rows it can render instead of copying the entire display list on
/// every GPUI render.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockedUserPage {
    pub users: Vec<BlockedUser>,
    pub total: usize,
    pub page: usize,
    pub page_count: usize,
}

#[derive(Clone)]
pub(crate) struct ActiveAuthSession {
    auth_epoch: u64,
    access_token: String,
    owner_user_id: String,
}

impl ActiveAuthSession {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn owner_user_id(&self) -> &str {
        &self.owner_user_id
    }
}

pub(crate) struct RefreshTicket {
    session: ActiveAuthSession,
    request_id: u64,
    mutation_revision: u64,
}

#[derive(Clone, Default)]
struct BlockedMembership {
    display_users: Vec<BlockedUser>,
    login_by_user_id: HashMap<String, String>,
    user_logins: HashSet<String>,
}

impl BlockedMembership {
    fn len(&self) -> usize {
        self.login_by_user_id.len()
    }

    fn display_truncated(&self) -> bool {
        self.len() > self.display_users.len()
    }

    fn sort_display_users(&mut self) {
        self.display_users.sort_by(|left, right| {
            left.display_name
                .to_ascii_lowercase()
                .cmp(&right.display_name.to_ascii_lowercase())
                .then_with(|| left.user_id.cmp(&right.user_id))
        });
    }

    fn remove_login_if_unused(&mut self, login: &str) {
        if !self
            .login_by_user_id
            .values()
            .any(|candidate| candidate == login)
        {
            self.user_logins.remove(login);
        }
    }

    fn apply_block_state(&mut self, user: &BlockedUser, blocked: bool) -> Result<(), String> {
        let user_id = user.user_id.trim();
        let user_login = user.user_login.trim().trim_start_matches('@');
        if user_id.is_empty() || user_login.is_empty() {
            return Err("Twitch blocked-user mutation returned an invalid identity".to_string());
        }
        let user_login = user_login.to_ascii_lowercase();
        if blocked
            && !self.login_by_user_id.contains_key(user_id)
            && self.login_by_user_id.len() >= MAX_CACHED_MEMBERSHIP
        {
            return Err(format!(
                "Twitch blocked-user membership exceeded the bounded {}-entry cache",
                MAX_CACHED_MEMBERSHIP
            ));
        }

        self.display_users.retain(|existing| {
            existing.user_id != user_id && !existing.user_login.eq_ignore_ascii_case(&user_login)
        });

        if let Some(previous_login) = self.login_by_user_id.remove(user_id) {
            self.remove_login_if_unused(&previous_login);
        }

        if blocked {
            self.login_by_user_id
                .insert(user_id.to_string(), user_login.clone());
            self.user_logins.insert(user_login.clone());
            if self.display_users.len() < MAX_DISPLAY_USERS {
                self.display_users.push(BlockedUser {
                    user_id: user_id.to_string(),
                    user_login,
                    display_name: user.display_name.trim().to_string(),
                });
                self.sort_display_users();
            }
        } else {
            self.remove_login_if_unused(&user_login);
        }

        Ok(())
    }
}

/// One immutable lock-free view used for a complete reconciliation pass. The
/// Arc keeps membership stable while producers publish a newer snapshot.
#[derive(Clone)]
pub(crate) struct BlockedVisibilityPolicy {
    membership: Arc<BlockedMembership>,
    pub visibility: BlockedUserVisibility,
}

impl BlockedVisibilityPolicy {
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.membership.login_by_user_id.is_empty()
    }

    #[inline]
    pub(crate) fn contains(&self, user_id: Option<&str>, user_login: &str) -> bool {
        membership_contains(&self.membership, user_id, user_login)
    }
}

struct LocalMutation {
    revision: u64,
    user: BlockedUser,
    blocked: bool,
}

#[derive(Default)]
struct BlockedUserCache {
    active_session: Option<ActiveAuthSession>,
    membership_owner_user_id: Option<String>,
    membership: Arc<BlockedMembership>,
    membership_complete: bool,
    mutation_revision: u64,
    local_mutations: VecDeque<LocalMutation>,
    status: BlockedUsersStatus,
}

#[derive(Deserialize)]
struct HelixBlockedUsersResponse {
    #[serde(default)]
    data: Vec<HelixBlockedUser>,
    #[serde(default)]
    pagination: HelixPagination,
}

#[derive(Deserialize)]
struct HelixBlockedUser {
    user_id: String,
    user_login: String,
    display_name: String,
}

#[derive(Default, Deserialize)]
struct HelixPagination {
    cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Applied(usize),
    Superseded,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BlockedUsersStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded {
        count: usize,
        truncated: bool,
    },
    Failed(String),
}

struct BlockedUsersFetch {
    membership: BlockedMembership,
}

/// Clear account-scoped state immediately and fence any in-flight request.
/// The returned epoch can only be activated after the token's account ID is
/// known. Keeping this separate from refresh sequencing prevents one manual
/// refresh from impersonating an authentication transition.
pub fn reset_for_auth_change() -> u64 {
    let auth_epoch = begin_auth_transition(false);
    finish_auth_transition_without_session(auth_epoch);
    auth_epoch
}

/// End a deliberately tokenless/signed-out transition without surfacing a
/// misleading failure. This releases the conservative visibility transaction
/// and prevents a persisted invalid token from stranding rows in NotLoaded.
pub(crate) fn finish_auth_transition_without_session(auth_epoch: u64) {
    let visibility_commit = visibility_commit_guard();
    let current = {
        let mut cache = CACHE.write_or_recover();
        if AUTH_EPOCH.load(Ordering::Acquire) == auth_epoch && cache.active_session.is_none() {
            cache.status = BlockedUsersStatus::NotLoaded;
            true
        } else {
            false
        }
    };
    let effects = current.then(|| {
        crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
            &visibility_commit,
            auth_epoch,
        )
    });
    drop(visibility_commit);
    if let Some(effects) = effects {
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    }
    if current {
        request_settings_refresh();
    }
}

/// Fence an authentication transition before its first await. Set
/// `preserve_last_good` only for a token rotation of the same account.
pub(crate) fn begin_auth_transition(preserve_last_good: bool) -> u64 {
    let visibility_commit = visibility_commit_guard();
    let auth_epoch = AUTH_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    REFRESH_SEQUENCE.fetch_add(1, Ordering::AcqRel);
    let abandoned_rows = clear_pending_visibility_commit_held(&visibility_commit);
    let (retains_authoritative_membership, retired_membership) = {
        let mut cache = CACHE.write_or_recover();
        let retains_authoritative_membership = preserve_last_good && cache.membership_complete;
        cache.active_session = None;
        cache.local_mutations.clear();
        if !retains_authoritative_membership {
            *cache = BlockedUserCache::default();
        }
        let retired_membership = PUBLISHED_MEMBERSHIP.swap(Arc::clone(&cache.membership));
        (retains_authoritative_membership, retired_membership)
    };
    let whisper_clear = (!retains_authoritative_membership).then(|| {
        crate::backend::whispers::clear_account_state_visibility_commit_held(&visibility_commit)
    });
    let must_establish_authority = crate::backend::settings::get_enable_twitch_blocked_users()
        && !retains_authoritative_membership;
    INITIAL_VISIBILITY_PENDING.store(must_establish_authority, Ordering::Release);
    RESIDENT_VISIBILITY_REVOKED.store(must_establish_authority, Ordering::Release);
    crate::backend::irc::discard_pending_blocked_visibility_rows_commit_held(
        &visibility_commit,
        abandoned_rows,
    );
    drop(visibility_commit);
    drop(retired_membership);
    if let Some(whisper_clear) = whisper_clear {
        crate::backend::whispers::dispatch_account_clear(whisper_clear);
    }
    if must_establish_authority {
        crate::backend::irc::notify_blocked_resident_visibility_revoked();
    }
    request_settings_refresh();
    auth_epoch
}

pub(crate) fn is_auth_epoch_current(auth_epoch: u64) -> bool {
    AUTH_EPOCH.load(Ordering::Acquire) == auth_epoch
}

/// Current account-visibility epoch. Callers use this only while already
/// holding the visibility commit gate to release staged rows when integration
/// is deliberately disabled (a fail-open policy transition).
pub(crate) fn current_auth_epoch_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) -> u64 {
    AUTH_EPOCH.load(Ordering::Acquire)
}

/// Publish the coherent token/account identity for a previously fenced epoch.
/// Anonymous mode and token changes are rejected before the session becomes
/// visible to manual refreshes or mutations.
pub(crate) fn activate_auth_session(
    auth_epoch: u64,
    access_token: &str,
    owner_user_id: &str,
) -> Result<ActiveAuthSession, String> {
    let access_token = normalize_oauth_token(access_token).trim().to_string();
    let owner_user_id = owner_user_id.trim().to_string();
    if access_token.is_empty() || owner_user_id.is_empty() {
        return Err("Twitch blocked users require an authenticated account identity".to_string());
    }
    if is_anonymous() {
        return Err("Twitch blocked users are unavailable in anonymous mode".to_string());
    }
    if !active_token_matches(&access_token) {
        return Err(
            "The active Twitch account changed before blocked users could load".to_string(),
        );
    }

    let session = ActiveAuthSession {
        auth_epoch,
        access_token,
        owner_user_id: owner_user_id.clone(),
    };
    let visibility_commit = visibility_commit_guard();
    let (owner_changed, retired_membership) = {
        let mut cache = CACHE.write_or_recover();
        if AUTH_EPOCH.load(Ordering::Acquire) != auth_epoch {
            return Err(
                "The active Twitch account changed before blocked users could load".to_string(),
            );
        }
        let owner_changed = cache
            .membership_owner_user_id
            .as_deref()
            .is_some_and(|owner| owner != owner_user_id);
        if owner_changed {
            *cache = BlockedUserCache::default();
        }
        cache.membership_owner_user_id = Some(owner_user_id);
        cache.active_session = Some(session.clone());
        let retired_membership =
            owner_changed.then(|| PUBLISHED_MEMBERSHIP.swap(Arc::clone(&cache.membership)));
        (owner_changed, retired_membership)
    };
    if owner_changed {
        INITIAL_VISIBILITY_PENDING.store(true, Ordering::Release);
        RESIDENT_VISIBILITY_REVOKED.store(true, Ordering::Release);
    }
    drop(visibility_commit);
    drop(retired_membership);
    if owner_changed {
        crate::backend::irc::notify_blocked_resident_visibility_revoked();
    }

    Ok(session)
}

pub(crate) fn fail_auth_transition(auth_epoch: u64, error: impl Into<String>) {
    let error = error.into();
    let visibility_commit = visibility_commit_guard();
    let mut changed = false;
    {
        let mut cache = CACHE.write_or_recover();
        if AUTH_EPOCH.load(Ordering::Acquire) == auth_epoch && cache.active_session.is_none() {
            cache.status = BlockedUsersStatus::Failed(error);
            changed = true;
        }
    }
    let effects = changed.then(|| {
        crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
            &visibility_commit,
            auth_epoch,
        )
    });
    drop(visibility_commit);
    if let Some(effects) = effects {
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    }
    if changed {
        request_settings_refresh();
    }
}

pub(crate) fn capture_active_session() -> Result<ActiveAuthSession, String> {
    if is_anonymous() {
        return Err("Twitch blocked users are unavailable in anonymous mode".to_string());
    }
    let session = CACHE
        .read_or_recover()
        .active_session
        .clone()
        .ok_or_else(|| {
            "The active Twitch account is still establishing its blocked-user session".to_string()
        })?;
    if !session_is_current(&session) {
        return Err("The active Twitch account changed; try again".to_string());
    }
    Ok(session)
}

pub(crate) fn session_is_current(session: &ActiveAuthSession) -> bool {
    if is_anonymous() || AUTH_EPOCH.load(Ordering::Acquire) != session.auth_epoch {
        return false;
    }
    let cache = CACHE.read_or_recover();
    session_matches_locked(&cache, session)
}

/// Supersede older refresh requests without changing the authentication epoch.
/// The current complete membership remains available until publication.
pub(crate) fn begin_refresh(session: &ActiveAuthSession) -> Option<RefreshTicket> {
    if is_anonymous() {
        return None;
    }
    let ticket = {
        let mut cache = CACHE.write_or_recover();
        if !session_matches_locked(&cache, session) {
            return None;
        }
        let request_id = REFRESH_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1;
        cache.status = BlockedUsersStatus::Loading;
        RefreshTicket {
            session: session.clone(),
            request_id,
            mutation_revision: cache.mutation_revision,
        }
    };
    request_settings_refresh();
    Some(ticket)
}

pub fn snapshot_page(requested_page: usize, page_size: usize) -> BlockedUserPage {
    if is_anonymous() {
        return BlockedUserPage {
            users: Vec::new(),
            total: 0,
            page: 0,
            page_count: 0,
        };
    }
    let cache = CACHE.read_or_recover();
    let display_users = &cache.membership.display_users;
    let total = display_users.len();
    let page_size = page_size.max(1);
    let page_count = total.div_ceil(page_size);
    let page = requested_page.min(page_count.saturating_sub(1));
    let start = page.saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);

    BlockedUserPage {
        users: display_users[start..end].to_vec(),
        total,
        page,
        page_count,
    }
}

pub fn status() -> BlockedUsersStatus {
    if is_anonymous() {
        return BlockedUsersStatus::NotLoaded;
    }
    CACHE.read_or_recover().status.clone()
}

/// Capture the complete immutable policy once for a bulk prune. Settings are
/// atomic mirrors and membership is ArcSwap-backed, so this takes no mutexes.
pub(crate) fn visibility_policy_snapshot() -> Option<BlockedVisibilityPolicy> {
    if !crate::backend::settings::get_enable_twitch_blocked_users() || is_anonymous() {
        return None;
    }
    Some(BlockedVisibilityPolicy {
        membership: PUBLISHED_MEMBERSHIP.load_full(),
        visibility: crate::backend::settings::get_show_blocked_users_messages(),
    })
}

pub fn is_blocked(user_id: Option<&str>, user_login: &str) -> bool {
    let membership = PUBLISHED_MEMBERSHIP.load();
    membership_contains(&membership, user_id, user_login)
}

/// Return the current remote block state when the target is safe to expose as
/// a user-card action. Twitch mutations require a stable target ID; self
/// actions are suppressed by ID with a defensive login fallback.
pub(crate) fn block_action_state(user_id: &str, user_login: &str) -> Option<bool> {
    let user_id = user_id.trim();
    let user_login = user_login.trim().trim_start_matches('@');
    if user_id.is_empty() || user_login.is_empty() {
        return None;
    }

    let session = capture_active_session().ok()?;
    let active_login = crate::backend::settings::get_username_lowercase();
    if user_id == session.owner_user_id()
        || (!active_login.is_empty() && user_login.eq_ignore_ascii_case(&active_login))
    {
        return None;
    }

    Some(is_blocked(Some(user_id), user_login))
}

#[inline]
fn membership_contains(
    membership: &BlockedMembership,
    user_id: Option<&str>,
    user_login: &str,
) -> bool {
    if user_id
        .filter(|id| !id.is_empty())
        .is_some_and(|id| membership.login_by_user_id.contains_key(id))
    {
        return true;
    }

    let login = user_login.trim().trim_start_matches('@');
    if login.is_empty() {
        return false;
    }
    if membership.user_logins.contains(login) {
        return true;
    }
    login
        .bytes()
        .any(|byte| byte.is_ascii_uppercase())
        .then(|| login.to_ascii_lowercase())
        .is_some_and(|login| membership.user_logins.contains(login.as_str()))
}

/// Whispers have no channel moderation context, so Twitch-blocked senders are
/// always suppressed while blocked-user integration is enabled.
pub fn should_hide_whisper(user_id: Option<&str>, user_login: &str) -> bool {
    crate::backend::settings::get_enable_twitch_blocked_users() && is_blocked(user_id, user_login)
}

/// Irreversible whisper effects must wait for the first complete membership
/// baseline. Later refreshes retain the last-good complete membership, while a
/// visible Failed state intentionally fails open instead of silencing whispers.
pub(crate) fn initial_visibility_pending() -> bool {
    INITIAL_VISIBILITY_PENDING.load(Ordering::Acquire)
        && crate::backend::settings::get_enable_twitch_blocked_users()
        && !is_anonymous()
}

/// Full account transitions hide resident rows as well as newly staged rows
/// until the new account's first membership attempt reaches a terminal state.
#[inline]
pub(crate) fn resident_visibility_revoked() -> bool {
    RESIDENT_VISIBILITY_REVOKED.load(Ordering::Acquire)
        && crate::backend::settings::get_enable_twitch_blocked_users()
        && !is_anonymous()
}

/// Mark channels whose authoritative backend buffer changed but whose GPUI
/// ChatState observer applies the refresh on a later frame. ChatView reads the
/// backend snapshot during this handoff so stale rows cannot flash back.
pub(crate) fn mark_chat_state_reconcile_pending<'a>(
    channels: impl IntoIterator<Item = &'a String>,
) {
    let generation = CHAT_STATE_RECONCILE_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1;
    let mut pending = CHAT_STATE_RECONCILE_CHANNELS.lock();
    CHAT_STATE_RECONCILE_PENDING.store(true, Ordering::Release);
    pending.extend(
        channels
            .into_iter()
            .map(|channel| (channel.to_lowercase(), generation)),
    );
    CHAT_STATE_RECONCILE_PENDING.store(!pending.is_empty(), Ordering::Release);
}

#[inline]
pub(crate) fn chat_state_reconcile_pending(channel: &str) -> bool {
    chat_state_reconcile_generation(channel).is_some()
}

pub(crate) fn chat_state_reconcile_generation(channel: &str) -> Option<u64> {
    if !CHAT_STATE_RECONCILE_PENDING.load(Ordering::Acquire) {
        return None;
    }
    CHAT_STATE_RECONCILE_CHANNELS
        .lock()
        .get(&channel.to_lowercase())
        .copied()
}

pub(crate) fn acknowledge_chat_state_reconciled(channel: &str, generation: u64) {
    if !CHAT_STATE_RECONCILE_PENDING.load(Ordering::Acquire) {
        return;
    }
    let mut pending = CHAT_STATE_RECONCILE_CHANNELS.lock();
    let channel = channel.to_lowercase();
    if pending.get(&channel).copied() == Some(generation) {
        pending.remove(&channel);
    }
    if pending.is_empty() {
        CHAT_STATE_RECONCILE_PENDING.store(false, Ordering::Release);
    }
}

pub(crate) fn clear_chat_state_reconcile_channel(channel: &str) {
    if !CHAT_STATE_RECONCILE_PENDING.load(Ordering::Acquire) {
        return;
    }
    let mut pending = CHAT_STATE_RECONCILE_CHANNELS.lock();
    pending.remove(&channel.to_lowercase());
    if pending.is_empty() {
        CHAT_STATE_RECONCILE_PENDING.store(false, Ordering::Release);
    }
}

/// Fetch and atomically publish the complete bounded block list for one
/// coherent authentication session. A stale account or newer refresh can
/// never publish. Mutations that finish during the request are merged into the
/// fetched list under the publication lock.
pub(crate) async fn refresh(ticket: RefreshTicket) -> Result<RefreshOutcome, String> {
    if !refresh_ticket_is_current(&ticket) {
        return Ok(RefreshOutcome::Superseded);
    }

    let fetch = fetch_all(&ticket);
    let fetched = match tokio::time::timeout(REFRESH_TIMEOUT, fetch).await {
        Ok(Ok(Some(fetched))) => fetched,
        Ok(Ok(None)) => return Ok(RefreshOutcome::Superseded),
        Ok(Err(error)) => {
            return finish_refresh_error(&ticket, error);
        }
        Err(_) => {
            return finish_refresh_error(
                &ticket,
                "Twitch blocked-user refresh timed out".to_string(),
            );
        }
    };

    let visibility_commit = visibility_commit_guard();
    let mut publication_error = None;
    let mut retired_membership = None;
    let total_count;
    {
        let mut cache = CACHE.write_or_recover();
        if !refresh_ticket_matches_locked(&cache, &ticket) {
            return Ok(RefreshOutcome::Superseded);
        }

        let mut membership = fetched.membership;
        for mutation in cache
            .local_mutations
            .iter()
            .filter(|mutation| mutation.revision > ticket.mutation_revision)
        {
            if let Err(error) = membership.apply_block_state(&mutation.user, mutation.blocked) {
                publication_error = Some(error);
                break;
            }
        }

        if let Some(error) = publication_error.as_ref() {
            cache.status = BlockedUsersStatus::Failed(error.clone());
            total_count = 0;
        } else {
            cache
                .local_mutations
                .retain(|mutation| mutation.revision > ticket.mutation_revision);
            total_count = membership.len();
            let display_count = membership.display_users.len();
            let truncated = membership.display_truncated();
            let membership = Arc::new(membership);
            cache.membership = Arc::clone(&membership);
            retired_membership = Some(PUBLISHED_MEMBERSHIP.swap(membership));
            cache.membership_complete = true;
            cache.membership_owner_user_id = Some(ticket.session.owner_user_id.clone());
            cache.status = BlockedUsersStatus::Loaded {
                count: display_count,
                truncated,
            };
        }
    }

    if let Some(error) = publication_error {
        let effects = crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
            &visibility_commit,
            ticket.session.auth_epoch,
        );
        drop(visibility_commit);
        drop(retired_membership);
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
        request_settings_refresh();
        return Err(error);
    }

    let effects = crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
        &visibility_commit,
        ticket.session.auth_epoch,
    );
    drop(visibility_commit);
    drop(retired_membership);
    crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    request_settings_refresh();
    Ok(RefreshOutcome::Applied(total_count))
}

async fn fetch_all(ticket: &RefreshTicket) -> Result<Option<BlockedUsersFetch>, String> {
    let mut membership = BlockedMembership::default();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut pages = 0usize;

    loop {
        if !refresh_ticket_is_current(ticket) {
            return Ok(None);
        }
        if pages >= MAX_BLOCKED_PAGES {
            return Err(format!(
                "Twitch blocked-user membership exceeded the bounded {MAX_REMOTE_MEMBERSHIP}-entry cache"
            ));
        }
        pages += 1;

        let url = blocked_users_url(ticket.session.owner_user_id(), cursor.as_deref());
        let request = HTTP_CLIENT
            .get(url)
            .header(
                "Authorization",
                format!("Bearer {}", ticket.session.access_token()),
            )
            .header("Client-Id", CLIENT_ID);

        if !refresh_ticket_is_current(ticket) {
            return Ok(None);
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("Failed to fetch Twitch blocked users: {error}"))?;
        if !response.status().is_success() {
            if matches!(
                response.status(),
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) {
                return Err(
                    "Twitch blocked users require the user:read:blocked_users scope; re-authenticate this account"
                        .to_string(),
                );
            }
            return Err(format!(
                "Twitch blocked-user refresh failed ({})",
                response.status()
            ));
        }

        let page: HelixBlockedUsersResponse = read_bounded_json(response).await?;
        if !refresh_ticket_is_current(ticket) {
            return Ok(None);
        }

        for user in page.data {
            let user_id = user.user_id.trim().to_string();
            let user_login = user.user_login.trim().to_ascii_lowercase();
            if user_id.is_empty() || user_login.is_empty() {
                continue;
            }
            if membership.login_by_user_id.contains_key(&user_id) {
                continue;
            }
            if membership.len() >= MAX_REMOTE_MEMBERSHIP {
                return Err(format!(
                    "Twitch blocked-user membership exceeded the bounded {MAX_REMOTE_MEMBERSHIP}-entry cache"
                ));
            }
            let display_name = if user.display_name.trim().is_empty() {
                user_login.clone()
            } else {
                user.display_name.trim().to_string()
            };
            membership
                .login_by_user_id
                .insert(user_id.clone(), user_login.clone());
            membership.user_logins.insert(user_login.clone());
            if membership.display_users.len() < MAX_DISPLAY_USERS {
                membership.display_users.push(BlockedUser {
                    user_id,
                    user_login,
                    display_name,
                });
            }
        }

        let next_cursor = page
            .pagination
            .cursor
            .filter(|value| !value.trim().is_empty());
        if next_cursor
            .as_ref()
            .is_some_and(|next| !seen_cursors.insert(next.clone()))
        {
            return Err("Twitch blocked-user refresh repeated a pagination cursor".to_string());
        }
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    membership.sort_display_users();
    Ok(Some(BlockedUsersFetch { membership }))
}

fn blocked_users_url(owner_user_id: &str, after: Option<&str>) -> Url {
    let mut url = Url::parse("https://api.twitch.tv/helix/users/blocks")
        .expect("static Twitch blocked-users URL must be valid");
    let first = PAGE_SIZE.to_string();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("broadcaster_id", owner_user_id)
            .append_pair("first", &first);
        if let Some(after) = after {
            query.append_pair("after", after);
        }
    }
    url
}

#[cfg(test)]
mod blocked_users_url_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn omits_cursor_on_the_initial_page() {
        let url = blocked_users_url("123456", None);
        assert_eq!(
            url.as_str(),
            "https://api.twitch.tv/helix/users/blocks?broadcaster_id=123456&first=100"
        );
        assert_eq!(url.query_pairs().count(), 2);
    }

    #[test]
    fn preserves_blocked_user_query_order_and_opaque_cursor_value() {
        let url = blocked_users_url("123456", Some("cursor+/="));
        assert_eq!(
            url.as_str(),
            "https://api.twitch.tv/helix/users/blocks?broadcaster_id=123456&first=100&after=cursor%2B%2F%3D"
        );
        assert_eq!(
            url.query_pairs().into_owned().collect::<Vec<_>>(),
            vec![
                ("broadcaster_id".to_string(), "123456".to_string()),
                ("first".to_string(), "100".to_string()),
                ("after".to_string(), "cursor+/=".to_string()),
            ]
        );
    }
}

async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAGE_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "Twitch blocked-user response exceeded the {} KiB limit",
            MAX_PAGE_RESPONSE_BYTES / 1024
        ));
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(MAX_PAGE_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed to read Twitch blocked users: {error}"))?
    {
        if chunk.len() > MAX_PAGE_RESPONSE_BYTES - body.len() {
            return Err(format!(
                "Twitch blocked-user response exceeded the {} KiB limit",
                MAX_PAGE_RESPONSE_BYTES / 1024
            ));
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body)
        .map_err(|error| format!("Failed to parse Twitch blocked users: {error}"))
}

async fn read_mutation_error_response(mut response: reqwest::Response) -> String {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MUTATION_ERROR_RESPONSE_BYTES as u64)
    {
        return format!(
            "response exceeded the {} KiB limit",
            MAX_MUTATION_ERROR_RESPONSE_BYTES / 1024
        );
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(MAX_MUTATION_ERROR_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(capacity);
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= MAX_MUTATION_ERROR_RESPONSE_BYTES - body.len() => {
                body.extend_from_slice(&chunk);
            }
            Ok(Some(_)) => {
                return format!(
                    "response exceeded the {} KiB limit",
                    MAX_MUTATION_ERROR_RESPONSE_BYTES / 1024
                );
            }
            Ok(None) => return String::from_utf8_lossy(&body).trim().to_string(),
            Err(error) => return format!("failed to read Twitch response: {error}"),
        }
    }
}

/// Update the local cache after a successful remote mutation. The session is
/// rechecked under the write lock and the bounded mutation journal is merged
/// into any older in-flight refresh at publication.
pub(crate) fn apply_block_state_if_current(
    session: &ActiveAuthSession,
    user_id: &str,
    user_login: &str,
    display_name: Option<&str>,
    blocked: bool,
) -> Result<(), String> {
    if is_anonymous() {
        return Err("Twitch blocked users are unavailable in anonymous mode".to_string());
    }
    let user_id = user_id.trim();
    let user_login = user_login.trim().trim_start_matches('@');
    if user_id.is_empty() || user_login.is_empty() {
        return Err("Twitch blocked-user mutation returned an invalid identity".to_string());
    }
    let user = BlockedUser {
        user_id: user_id.to_string(),
        user_login: user_login.to_ascii_lowercase(),
        display_name: display_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(user_login)
            .trim()
            .to_string(),
    };

    let visibility_commit = visibility_commit_guard();
    let (reconcile_incomplete_membership, retired_membership) = {
        let mut cache = CACHE.write_or_recover();
        if !mutation_session_matches_locked(&cache, session) {
            return Err(
                "The active Twitch account changed while the request was running".to_string(),
            );
        }
        Arc::make_mut(&mut cache.membership).apply_block_state(&user, blocked)?;
        let retired_membership = PUBLISHED_MEMBERSHIP.swap(Arc::clone(&cache.membership));
        cache.mutation_revision = cache.mutation_revision.wrapping_add(1).max(1);
        let revision = cache.mutation_revision;
        cache
            .local_mutations
            .retain(|mutation| mutation.user.user_id != user.user_id);
        let mut invalidated_refresh = false;
        if cache.local_mutations.len() >= MAX_LOCAL_MUTATIONS {
            REFRESH_SEQUENCE.fetch_add(1, Ordering::AcqRel);
            cache.local_mutations.clear();
            invalidated_refresh = true;
        } else {
            cache.local_mutations.push_back(LocalMutation {
                revision,
                user,
                blocked,
            });
        }
        if cache.membership_complete {
            cache.status = BlockedUsersStatus::Loaded {
                count: cache.membership.display_users.len(),
                truncated: cache.membership.display_truncated(),
            };
        }
        (
            !cache.membership_complete
                && (invalidated_refresh || !matches!(cache.status, BlockedUsersStatus::Loading)),
            retired_membership,
        )
    };

    let changed_channels = if blocked {
        crate::backend::irc::apply_blocked_user_filter_to_buffers_visibility_commit_held(
            &visibility_commit,
        )
    } else {
        HashSet::new()
    };
    drop(visibility_commit);
    drop(retired_membership);
    crate::backend::irc::dispatch_blocked_channel_refreshes(changed_channels);
    request_settings_refresh();
    if reconcile_incomplete_membership {
        spawn_reconciliation_refresh(session.clone());
    }
    Ok(())
}

/// Apply one Twitch block mutation through the account session captured by
/// the caller. Every surface (commands, Settings, and user cards) shares this
/// bounded request path so account fencing and local publication cannot drift.
/// Why a block or unblock did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockStateFailure {
    /// The token lacks `user:manage:blocked_users`.
    NeedsReauth,
    TimedOut,
    /// The active account changed before or while the request ran.
    AccountChanged,
    Failed,
}

/// A failed block or unblock: its cause and a message for the user.
#[derive(Debug)]
pub(crate) struct BlockStateError {
    kind: BlockStateFailure,
    message: String,
}

impl BlockStateError {
    fn new(kind: BlockStateFailure, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new(BlockStateFailure::Failed, message)
    }

    pub(crate) fn kind(&self) -> BlockStateFailure {
        self.kind
    }
}

impl std::fmt::Display for BlockStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<BlockStateError> for String {
    fn from(error: BlockStateError) -> Self {
        error.message
    }
}

pub(crate) async fn set_block_state_for_session(
    session: ActiveAuthSession,
    user: BlockedUser,
    blocked: bool,
) -> Result<(), BlockStateError> {
    let user_id = user.user_id.trim();
    let user_login = user.user_login.trim().trim_start_matches('@');
    if user_id.is_empty() || user_login.is_empty() {
        return Err(BlockStateError::failed(
            "A stable Twitch user identity is required",
        ));
    }

    let active_login = crate::backend::settings::get_username_lowercase();
    if user_id == session.owner_user_id()
        || (!active_login.is_empty() && user_login.eq_ignore_ascii_case(&active_login))
    {
        return Err(BlockStateError::failed(
            "The active Twitch account cannot block itself",
        ));
    }
    if !session_is_current(&session) {
        return Err(BlockStateError::new(
            BlockStateFailure::AccountChanged,
            "The active Twitch account changed before the request started",
        ));
    }

    let url = format!(
        "https://api.twitch.tv/helix/users/blocks?target_user_id={}",
        urlencoding::encode(user_id)
    );
    let request = if blocked {
        HTTP_CLIENT.put(url)
    } else {
        HTTP_CLIENT.delete(url)
    }
    .header(
        "Authorization",
        format!("Bearer {}", session.access_token()),
    )
    .header("Client-Id", CLIENT_ID)
    .send();

    let remote_result = tokio::time::timeout(MUTATION_TIMEOUT, async move {
        let response = request.await.map_err(|error| {
            let action = if blocked { "Block" } else { "Unblock" };
            let kind = if error.is_timeout() {
                BlockStateFailure::TimedOut
            } else {
                BlockStateFailure::Failed
            };
            BlockStateError::new(
                kind,
                format!("{action} request failed: {}", error.without_url()),
            )
        })?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(BlockStateError::new(
                BlockStateFailure::NeedsReauth,
                "Re-authenticate this account to grant user:manage:blocked_users",
            ));
        }
        let detail = read_mutation_error_response(response).await;
        let action = if blocked { "block" } else { "unblock" };
        Err(BlockStateError::failed(if detail.is_empty() {
            format!("Twitch failed to {action} the user ({status})")
        } else {
            format!("Twitch failed to {action} the user ({status}): {detail}")
        }))
    })
    .await;
    match remote_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            let action = if blocked { "Block" } else { "Unblock" };
            return Err(BlockStateError::new(
                BlockStateFailure::TimedOut,
                format!("{action} request timed out"),
            ));
        }
    }

    apply_block_state_if_current(
        &session,
        user_id,
        user_login,
        Some(&user.display_name),
        blocked,
    )
    .map_err(|error| {
        BlockStateError::new(
            BlockStateFailure::AccountChanged,
            format!("Twitch completed the request for the previous account: {error}"),
        )
    })
}

/// Spawn a UI-originated mutation while retaining its exact account session.
/// Toast copy intentionally omits the target identity so streamer mode can be
/// enabled while the request is in flight without exposing it afterward.
pub(crate) fn set_block_state_async_for_session(
    session: ActiveAuthSession,
    user: BlockedUser,
    blocked: bool,
) {
    crate::backend::bridge::get_runtime().spawn(async move {
        match set_block_state_for_session(session, user, blocked).await {
            Ok(()) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: if blocked {
                        "User blocked on Twitch".to_string()
                    } else {
                        "User unblocked on Twitch".to_string()
                    },
                    detail: None,
                });
            }
            Err(error) => {
                let detail = match error.kind() {
                    BlockStateFailure::NeedsReauth => {
                        "Re-authenticate the active account and try again."
                    }
                    BlockStateFailure::TimedOut => "The Twitch request timed out.",
                    BlockStateFailure::AccountChanged => {
                        "The active Twitch account changed; try again."
                    }
                    BlockStateFailure::Failed => "Twitch could not complete the request.",
                };
                push_warning(
                    if blocked {
                        "Block failed"
                    } else {
                        "Unblock failed"
                    },
                    Some(detail.to_string()),
                );
            }
        }
    });
}

fn spawn_reconciliation_refresh(session: ActiveAuthSession) {
    let Some(ticket) = begin_refresh(&session) else {
        return;
    };
    crate::backend::bridge::get_runtime().spawn(async move {
        match refresh(ticket).await {
            Ok(RefreshOutcome::Applied(count)) => crate::log_auth!(
                info,
                "Reconciled {} Twitch blocked user(s) after a mutation",
                count
            ),
            Ok(RefreshOutcome::Superseded) => {}
            Err(error) => crate::log_auth!(
                warn,
                "Failed to reconcile Twitch blocked users after a mutation: {}",
                error
            ),
        }
    });
}

/// Refresh the active account from the Filters settings surface.
pub fn refresh_current_account_async() {
    if let Ok(session) = capture_active_session() {
        spawn_settings_refresh(session);
        return;
    }

    if is_anonymous() {
        push_warning(
            "Blocked-user refresh unavailable",
            Some("Twitch blocked users are unavailable in anonymous mode".to_string()),
        );
        return;
    }

    let auth_epoch = begin_auth_transition(false);
    let Some(access_token) = crate::backend::settings::get_token()
        .map(|token| normalize_oauth_token(&token).trim().to_string())
        .filter(|token| !token.is_empty())
    else {
        let error = "The active Twitch account has no access token".to_string();
        fail_auth_transition(auth_epoch, error.clone());
        push_warning("Blocked-user refresh unavailable", Some(error));
        return;
    };

    mark_auth_transition_loading(auth_epoch);
    crate::backend::bridge::get_runtime().spawn(async move {
        let result = recover_session_and_refresh(access_token, auth_epoch).await;
        report_settings_refresh_result(result);
    });
}

fn mark_auth_transition_loading(auth_epoch: u64) {
    let changed = {
        let mut cache = CACHE.write_or_recover();
        if AUTH_EPOCH.load(Ordering::Acquire) != auth_epoch {
            false
        } else {
            cache.status = BlockedUsersStatus::Loading;
            true
        }
    };
    if changed {
        request_settings_refresh();
    }
}

fn spawn_settings_refresh(session: ActiveAuthSession) {
    let Some(ticket) = begin_refresh(&session) else {
        push_warning(
            "Blocked-user refresh unavailable",
            Some("The active Twitch account changed; try again.".to_string()),
        );
        return;
    };

    crate::backend::bridge::get_runtime().spawn(async move {
        report_settings_refresh_result(refresh(ticket).await);
    });
}

async fn recover_session_and_refresh(
    mut access_token: String,
    auth_epoch: u64,
) -> Result<RefreshOutcome, String> {
    for attempt in 0..MAX_MANUAL_AUTH_RECOVERY_ATTEMPTS {
        if is_anonymous() || !is_auth_epoch_current(auth_epoch) {
            return Ok(RefreshOutcome::Superseded);
        }

        let validation = crate::backend::auth::validate_token_with_user_id(&access_token).await;
        if is_anonymous() || !is_auth_epoch_current(auth_epoch) {
            return Ok(RefreshOutcome::Superseded);
        }

        if !active_token_matches(&access_token) {
            let next_token = crate::backend::settings::get_token()
                .map(|token| normalize_oauth_token(&token).trim().to_string())
                .filter(|token| !token.is_empty());
            if let Some(next_token) = next_token.filter(|token| token != &access_token)
                && attempt + 1 < MAX_MANUAL_AUTH_RECOVERY_ATTEMPTS
            {
                access_token = next_token;
                continue;
            }

            return fail_manual_auth_recovery(
                auth_epoch,
                "The active Twitch account changed repeatedly while blocked users were refreshing",
            );
        }

        let owner_user_id = match validation {
            Ok(Some(user_id)) if !user_id.trim().is_empty() => user_id,
            Ok(_) => {
                return fail_manual_auth_recovery(
                    auth_epoch,
                    "Twitch could not validate the active account identity or required scopes",
                );
            }
            Err(error) => {
                if is_auth_epoch_current(auth_epoch) {
                    fail_auth_transition(auth_epoch, error.clone());
                    return Err(error);
                }
                return Ok(RefreshOutcome::Superseded);
            }
        };

        let session = match activate_auth_session(auth_epoch, &access_token, &owner_user_id) {
            Ok(session) => session,
            Err(_) if !is_auth_epoch_current(auth_epoch) => {
                return Ok(RefreshOutcome::Superseded);
            }
            Err(_) if !active_token_matches(&access_token) => {
                let Some(next_token) = crate::backend::settings::get_token()
                    .map(|token| normalize_oauth_token(&token).trim().to_string())
                    .filter(|token| !token.is_empty())
                else {
                    return fail_manual_auth_recovery(
                        auth_epoch,
                        "The active Twitch account has no access token",
                    );
                };
                if attempt + 1 < MAX_MANUAL_AUTH_RECOVERY_ATTEMPTS {
                    access_token = next_token;
                    continue;
                }
                return fail_manual_auth_recovery(
                    auth_epoch,
                    "The active Twitch account changed repeatedly while blocked users were refreshing",
                );
            }
            Err(error) => {
                fail_auth_transition(auth_epoch, error.clone());
                return Err(error);
            }
        };
        let Some(ticket) = begin_refresh(&session) else {
            if is_auth_epoch_current(auth_epoch) {
                return fail_manual_auth_recovery(
                    auth_epoch,
                    "The active Twitch account changed before blocked users could refresh",
                );
            }
            return Ok(RefreshOutcome::Superseded);
        };
        return refresh(ticket).await;
    }

    fail_manual_auth_recovery(
        auth_epoch,
        "The active Twitch account changed repeatedly while blocked users were refreshing",
    )
}

fn fail_manual_auth_recovery(
    auth_epoch: u64,
    error: impl Into<String>,
) -> Result<RefreshOutcome, String> {
    if !is_auth_epoch_current(auth_epoch) {
        return Ok(RefreshOutcome::Superseded);
    }
    let error = error.into();
    fail_auth_transition(auth_epoch, error.clone());
    Err(error)
}

fn report_settings_refresh_result(result: Result<RefreshOutcome, String>) {
    match result {
        Ok(RefreshOutcome::Applied(count)) => {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                message: "Twitch blocked users refreshed".to_string(),
                detail: Some(format!("Loaded {count} blocked user(s).")),
            });
        }
        Ok(RefreshOutcome::Superseded) => {}
        Err(error) => push_warning("Blocked-user refresh failed", Some(error)),
    }
}

/// Remove a cached user from the active Twitch account from Settings.
pub fn unblock_user_async(user: BlockedUser) {
    let session = match capture_active_session() {
        Ok(session) => session,
        Err(error) => {
            push_warning("Unblock unavailable", Some(error));
            return;
        }
    };
    set_block_state_async_for_session(session, user, false);
}

fn push_warning(message: &str, detail: Option<String>) {
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: message.to_string(),
        detail,
    });
}

fn active_token_matches(token: &str) -> bool {
    crate::backend::settings::get_token()
        .map(|active| normalize_oauth_token(&active) == normalize_oauth_token(token))
        .unwrap_or(false)
}

fn is_anonymous() -> bool {
    crate::backend::irc::is_anonymous_mode()
}

fn session_matches_locked(cache: &BlockedUserCache, session: &ActiveAuthSession) -> bool {
    AUTH_EPOCH.load(Ordering::Acquire) == session.auth_epoch
        && cache.active_session.as_ref().is_some_and(|active| {
            active.auth_epoch == session.auth_epoch
                && active.owner_user_id == session.owner_user_id
                && active.access_token == session.access_token
        })
}

/// A remote block mutation that completed during a same-owner token rotation
/// still changed Twitch successfully. Accept it into the retained membership
/// and mutation journal; only a different account owner invalidates it.
fn mutation_session_matches_locked(cache: &BlockedUserCache, session: &ActiveAuthSession) -> bool {
    session_matches_locked(cache, session)
        || (cache.membership_complete
            && cache.membership_owner_user_id.as_deref() == Some(session.owner_user_id())
            && cache
                .active_session
                .as_ref()
                .is_none_or(|active| active.owner_user_id == session.owner_user_id))
}

fn refresh_ticket_matches_locked(cache: &BlockedUserCache, ticket: &RefreshTicket) -> bool {
    REFRESH_SEQUENCE.load(Ordering::Acquire) == ticket.request_id
        && session_matches_locked(cache, &ticket.session)
}

fn refresh_ticket_is_current(ticket: &RefreshTicket) -> bool {
    if is_anonymous() {
        return false;
    }
    let cache = CACHE.read_or_recover();
    refresh_ticket_matches_locked(&cache, ticket)
}

fn finish_refresh_error(ticket: &RefreshTicket, error: String) -> Result<RefreshOutcome, String> {
    let visibility_commit = visibility_commit_guard();
    let changed = {
        let mut cache = CACHE.write_or_recover();
        if !refresh_ticket_matches_locked(&cache, ticket) {
            false
        } else {
            cache.status = BlockedUsersStatus::Failed(error.clone());
            true
        }
    };
    let effects = changed.then(|| {
        crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
            &visibility_commit,
            ticket.session.auth_epoch,
        )
    });
    drop(visibility_commit);
    if let Some(effects) = effects {
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    }
    if changed {
        request_settings_refresh();
        Err(error)
    } else {
        Ok(RefreshOutcome::Superseded)
    }
}

fn request_settings_refresh() {
    crate::backend::settings::broadcast_settings_event(
        crate::backend::settings::SettingsEvent::FiltersChanged,
    );
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}
