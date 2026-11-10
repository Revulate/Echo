//! Authentication state and login flows
//!
//! Handles OAuth device flow, token refresh, and account management.

use super::{Account, AuthState, RUNTIME};
use crate::backend::lock_utils::RwLockExt;

/// Start OAuth login flow
pub fn start_login() {
    start_login_attempt();
}

pub fn start_login_attempt() -> crate::backend::auth::LoginAttempt {
    let attempt = crate::backend::auth::begin_login();
    let request_attempt = attempt.clone();
    RUNTIME.spawn(async move {
        crate::backend::auth::start_login_attempt(request_attempt).await;
    });
    attempt
}

/// Cancel the current login flow
pub fn cancel_login() {
    crate::backend::auth::cancel_login();
}

/// Logout from current account
pub fn logout() {
    RUNTIME.spawn_blocking(|| {
        RUNTIME.handle().block_on(async {
            if let Err(e) = crate::backend::auth::logout().await {
                crate::log_auth!(warn, "Logout failed: {}", e);
            }
        });
    });
}

/// Get current auth state
pub fn get_auth_state() -> AuthState {
    crate::backend::auth::AUTH_STATE.read_or_recover().clone()
}

/// Check if authenticated
pub fn is_authenticated() -> bool {
    crate::backend::auth::AUTH_STATE
        .read_or_recover()
        .authenticated
}

/// Switch to a different account
pub fn switch_account(username: &str) {
    let username = username.to_string();
    RUNTIME.spawn(async move {
        match crate::backend::auth::switch_account(&username).await {
            Ok(()) => {
                let detail = if crate::backend::streamer_mode::should_hide_account_names() {
                    None
                } else {
                    Some(format!("Active account is now @{username}"))
                };
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: "Twitch account switched".to_string(),
                    detail,
                });
            }
            Err(e) => {
                crate::log_auth!(warn, "Switch account failed: {}", e);
                let detail = if crate::backend::streamer_mode::should_hide_account_names() {
                    Some("Switch account failed".to_string())
                } else {
                    Some(format!("@{username}: {e}"))
                };
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Failed to switch Twitch account".to_string(),
                    detail,
                });
            }
        }
    });
}

/// Get all accounts
pub fn get_accounts() -> Vec<Account> {
    crate::backend::settings::get_accounts()
        .into_iter()
        .map(|a| a.into())
        .collect()
}

/// Remove an account
pub fn remove_account(username: &str) {
    let username = username.to_string();
    RUNTIME.spawn_blocking(move || {
        RUNTIME.handle().block_on(async move {
            match crate::backend::auth::remove_account(&username).await {
                Ok(()) => {
                    let detail = if crate::backend::streamer_mode::should_hide_account_names() {
                        None
                    } else {
                        Some(format!("Removed @{username}"))
                    };
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                        message: "Twitch account removed".to_string(),
                        detail,
                    });
                }
                Err(e) => {
                    crate::log_auth!(warn, "Remove account failed: {}", e);
                    let detail = if crate::backend::streamer_mode::should_hide_account_names() {
                        Some("Remove account failed".to_string())
                    } else {
                        Some(format!("@{username}: {e}"))
                    };
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                        message: "Failed to remove Twitch account".to_string(),
                        detail,
                    });
                }
            }
        });
    });
}

/// Initialize auth on app startup (validates token, refreshes if needed)
pub fn initialize_auth() {
    RUNTIME.spawn(async {
        crate::backend::auth::initialize_auth().await;
    });
}

/// Start the background token refresh monitor
pub fn start_token_refresh_monitor() {
    RUNTIME.spawn(async {
        crate::backend::auth::start_token_refresh_monitor_async().await;
    });
}

/// Check if re-authentication is needed (e.g., token permanently expired)
pub fn get_needs_reauth() -> bool {
    crate::backend::auth::get_needs_reauth()
}

/// Check whether the active saved account exists but has no hydrated runtime token.
pub fn active_account_missing_runtime_token() -> bool {
    crate::backend::settings::get_active_account()
        .is_some_and(|account| account.token.trim().is_empty())
}

/// Check whether a saved account exists but has no hydrated runtime token.
pub fn saved_account_missing_runtime_token(username: &str) -> bool {
    crate::backend::settings::get_accounts()
        .into_iter()
        .find(|account| account.username == username)
        .is_some_and(|account| account.token.trim().is_empty())
}

/// Check whether the active account needs a fresh OAuth login before it is usable.
pub fn active_account_needs_reauth() -> bool {
    get_needs_reauth() || active_account_missing_runtime_token()
}

/// Check if IRC is in anonymous mode
pub fn is_anonymous_mode() -> bool {
    crate::backend::irc::is_anonymous_mode()
}
