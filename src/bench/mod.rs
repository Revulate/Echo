//! Benchmark-only hooks that ship in every build, because the perf rigs
//! measure the distributed binary: the isolated-profile receipt, the
//! live-channel workload and its readiness probe, and the no-send guard.
//!
//! Storage isolation itself is product code and lives in
//! [`crate::storage_roots`].

pub mod live_channel;
mod receipt;
pub mod ui_fixture;

use std::sync::atomic::{AtomicU64, Ordering};

const OFFLINE_ENV: &str = "ECHO_BENCH_OFFLINE";

static OUTBOUND_USER_ACTION_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Validate the benchmark environment and publish the profile receipt.
/// Runs after [`crate::storage_roots::initialize`].
pub fn initialize() -> Result<(), String> {
    let Some(roots) = crate::storage_roots::profile_roots() else {
        return live_channel::initialize_without_profile();
    };
    live_channel::initialize(roots)?;
    receipt::write_profile_receipt(roots)
}

/// Reject a user-originating outbound action during any no-send benchmark.
/// Transport protocol traffic (IRC JOIN/PONG and connection supervision) never
/// calls this boundary. The counter is process-local and exists while either the
/// explicit live-channel contract or the isolated offline guard is active.
pub fn reject_live_outbound_user_action(action: &str) -> Result<(), String> {
    if !outbound_user_action_must_be_rejected(
        live_channel::requested_channel().is_some(),
        network_lanes_suppressed(),
    ) {
        return Ok(());
    }

    let attempt = OUTBOUND_USER_ACTION_ATTEMPTS.fetch_add(1, Ordering::AcqRel) + 1;
    Err(format!(
        "Benchmark no-send guard rejected outbound user action '{action}' (attempt {attempt})"
    ))
}

fn outbound_user_action_must_be_rejected(
    live_channel_active: bool,
    network_lanes_suppressed: bool,
) -> bool {
    live_channel_active || network_lanes_suppressed
}

pub fn live_outbound_user_action_attempts() -> u64 {
    OUTBOUND_USER_ACTION_ATTEMPTS.load(Ordering::Acquire)
}

/// Return whether every application-owned network lane must remain disabled.
///
/// `ECHO_BENCH_OFFLINE` is accepted only with an isolated benchmark profile,
/// so an ordinary user launch cannot accidentally inherit offline behavior.
pub fn network_lanes_suppressed() -> bool {
    offline_env_suppresses_network(
        crate::storage_roots::profile_root().is_some(),
        echo_core::env::value(OFFLINE_ENV).as_deref(),
    )
}

fn offline_env_suppresses_network(profile_active: bool, value: Option<&str>) -> bool {
    profile_active && value.is_some_and(echo_core::env::is_truthy)
}

#[cfg(test)]
mod tests {
    use super::{offline_env_suppresses_network, outbound_user_action_must_be_rejected};
    use core::prelude::v1::test;

    #[test]
    fn offline_and_live_benchmarks_reject_outbound_user_actions() {
        assert!(!outbound_user_action_must_be_rejected(false, false));
        assert!(outbound_user_action_must_be_rejected(true, false));
        assert!(outbound_user_action_must_be_rejected(false, true));
        assert!(outbound_user_action_must_be_rejected(true, true));
    }

    #[test]
    fn lightweight_offline_mode_requires_an_isolated_profile() {
        assert!(!offline_env_suppresses_network(false, Some("1")));
        assert!(!offline_env_suppresses_network(true, Some("false")));
        assert!(offline_env_suppresses_network(true, Some("1")));
        assert!(offline_env_suppresses_network(true, Some("yes")));
    }
}
