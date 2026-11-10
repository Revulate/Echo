//! State that render() reads without touching the backend: a cached
//! authentication snapshot and the queue of pending emote popout requests.

use crate::backend::lock_utils::{MutexExt, RwLockExt};
use std::sync::{LazyLock, Mutex, RwLock};

/// Cached auth state to avoid FFI calls every frame
static AUTH_CACHE: LazyLock<RwLock<CachedAuthState>> =
    LazyLock::new(|| RwLock::new(CachedAuthState::default()));

/// Cached authentication state
#[derive(Clone, Default)]
pub struct CachedAuthState {
    pub is_authenticated: bool,
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub profile_image_url: Option<String>,
    pub device_code: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    pub needs_reauth: bool,
    pub version: u64,
}

/// Get cached auth state (avoids FFI calls every frame)
/// Call refresh_auth_cache() periodically (e.g., every second) to update
pub fn get_cached_auth_state() -> CachedAuthState {
    AUTH_CACHE.read_or_recover().clone()
}

/// Refresh the auth cache (call periodically, not every frame)
pub fn refresh_auth_cache() {
    let auth_state = crate::backend::get_auth_state();
    let is_authenticated = crate::backend::is_authenticated();
    write_auth_cache(auth_state, is_authenticated);
}

/// Refresh the auth cache only when a visible auth field changed.
pub fn refresh_auth_cache_if_changed() -> bool {
    let auth_state = crate::backend::get_auth_state();
    let is_authenticated = crate::backend::is_authenticated();
    {
        let cache = AUTH_CACHE.read_or_recover();
        if cache.is_authenticated == is_authenticated
            && cache.display_name == auth_state.display_name
            && cache.username == auth_state.username
            && cache.profile_image_url == auth_state.profile_image_url
            && cache.device_code == auth_state.device_code
            && cache.user_code == auth_state.user_code
            && cache.verification_uri == auth_state.verification_uri
            && cache.error == auth_state.error
            && cache.loading == auth_state.loading
            && cache.needs_reauth == auth_state.needs_reauth
        {
            return false;
        }
    }

    write_auth_cache(auth_state, is_authenticated);
    true
}

fn write_auth_cache(auth_state: crate::backend::AuthState, is_authenticated: bool) {
    crate::state::VersionRegistry::bump_auth_version();
    let version = crate::state::VersionRegistry::auth_version();

    let mut cache = AUTH_CACHE.write_or_recover();
    cache.is_authenticated = is_authenticated;
    cache.display_name = auth_state.display_name;
    cache.username = auth_state.username;
    cache.profile_image_url = auth_state.profile_image_url;
    cache.device_code = auth_state.device_code;
    cache.user_code = auth_state.user_code;
    cache.verification_uri = auth_state.verification_uri;
    cache.error = auth_state.error;
    cache.loading = auth_state.loading;
    cache.needs_reauth = auth_state.needs_reauth;
    cache.version = version;
}

/// Queue for pending emote popout window requests (handled by polling loop)
static PENDING_POPOUT_REQUEST: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Queue a request to open an emote popout window
pub fn queue_emote_popout(channel: String) {
    PENDING_POPOUT_REQUEST.lock_or_recover().push(channel);
}

/// Take all pending popout requests
pub fn take_pending_popouts() -> Vec<String> {
    std::mem::take(&mut *PENDING_POPOUT_REQUEST.lock_or_recover())
}
