//! Per-split composer send state: whether the account can send in a channel
//! now (slow mode, a timeout, a ban) and the wording of a rejection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct ComposerSendState {
    pub channel: String,
    pub account: Option<String>,
    pending: HashSet<u64>,
    last_outcome: u64,
    notice: Option<String>,
}

impl ComposerSendState {
    pub fn new(channel: String, account: Option<String>) -> Self {
        Self {
            channel,
            account,
            ..Self::default()
        }
    }

    pub fn begin(&mut self, id: u64) {
        self.pending.insert(id);
        self.notice = None;
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn finish(&mut self, id: u64, notice: Option<String>) -> bool {
        if !self.pending.remove(&id) {
            return false;
        }

        if id >= self.last_outcome {
            self.last_outcome = id;
            self.notice = notice;
        } else if notice.is_some() && self.notice.is_none() {
            self.notice = notice;
        }
        true
    }

    pub fn text(&self) -> Option<String> {
        let sending = match self.pending.len() {
            0 => None,
            1 => Some("Sending…".to_string()),
            count => Some(format!("Sending {count}…")),
        };

        match (sending, self.notice.as_deref()) {
            (Some(sending), Some(notice)) => Some(format!("{sending} — {notice}")),
            (Some(sending), None) => Some(sending),
            (None, notice) => notice.map(str::to_string),
        }
    }

    pub fn matches(&self, channel: &str, account: &Option<String>) -> bool {
        self.channel
            .trim()
            .trim_start_matches('#')
            .eq_ignore_ascii_case(channel.trim().trim_start_matches('#'))
            && self.account == *account
    }
}

pub fn rejection_reason(code: &str, fallback: &str) -> String {
    if code.trim().eq_ignore_ascii_case("automod_held") {
        return "Held for moderator review — don’t resend".to_string();
    }

    let mut words = fallback.split_whitespace();
    let Some(first) = words.next() else {
        return "Message was not accepted".to_string();
    };
    let readable = std::iter::once(first)
        .chain(words)
        .collect::<Vec<_>>()
        .join(" ");
    const LIMIT: usize = 180;
    if readable.chars().count() <= LIMIT {
        readable
    } else {
        format!("{}…", readable.chars().take(LIMIT).collect::<String>())
    }
}

const MAX_ELIGIBILITY_ENTRIES: usize = 2048;
const MAX_SLOW_WAIT: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Default)]
pub struct SendEligibilityStore {
    entries: HashMap<(String, String), EligibilityEntry>,
}

struct EligibilityEntry {
    exempt: bool,
    exemption_known: bool,
    restriction: Option<EligibilityRestriction>,
    touched: Instant,
}

enum EligibilityRestriction {
    Slow(Instant),
    TimedOut(Instant),
    Banned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EligibilityBlock {
    Slow(u64),
    TimedOut(u64),
    Banned,
}

impl EligibilityBlock {
    pub fn text(self, channel: &str) -> String {
        match self {
            Self::Slow(seconds) => format!("Slow mode · {seconds} s"),
            Self::TimedOut(seconds) => format!("Timed out · {}:{:02}", seconds / 60, seconds % 60),
            Self::Banned => format!("Banned from #{}", normalized_channel(channel)),
        }
    }
}

pub struct EligibilityStatus {
    pub block: EligibilityBlock,
    pub next_change: Option<Duration>,
}

impl SendEligibilityStore {
    pub fn set_exempt(&mut self, account: &str, channel: &str, exempt: bool, now: Instant) -> bool {
        let entry = self.entry(account, channel, now);
        let changed = entry.exempt != exempt || !entry.exemption_known;
        entry.exempt = exempt;
        entry.exemption_known = true;
        if exempt && matches!(entry.restriction, Some(EligibilityRestriction::Slow(_))) {
            entry.restriction = None;
        }
        changed
    }

    pub fn exemption_known(&self, account: &str, channel: &str) -> bool {
        self.entries
            .get(&(normalized_account(account), normalized_channel(channel)))
            .is_some_and(|entry| entry.exemption_known)
    }

    pub fn confirmed_send(
        &mut self,
        account: &str,
        channel: &str,
        slow: Option<Duration>,
        now: Instant,
    ) -> bool {
        let Some(slow) = slow.filter(|slow| !slow.is_zero()) else {
            return false;
        };
        let entry = self.entry(account, channel, now);
        if entry.exempt || entry.restriction.is_some() {
            return false;
        }
        let Some(deadline) = now.checked_add(slow) else {
            return false;
        };
        entry.restriction = Some(EligibilityRestriction::Slow(deadline));
        true
    }

    pub fn server_slow(
        &mut self,
        account: &str,
        channel: &str,
        remaining: Duration,
        now: Instant,
    ) -> bool {
        if remaining.is_zero() {
            return false;
        }
        let entry = self.entry(account, channel, now);
        let was_exempt = std::mem::replace(&mut entry.exempt, false);
        if matches!(
            entry.restriction,
            Some(EligibilityRestriction::TimedOut(_) | EligibilityRestriction::Banned)
        ) {
            return was_exempt;
        }
        let Some(deadline) = now.checked_add(remaining) else {
            return false;
        };
        let changed = !matches!(entry.restriction, Some(EligibilityRestriction::Slow(existing)) if existing == deadline);
        entry.restriction = Some(EligibilityRestriction::Slow(deadline));
        was_exempt || changed
    }

    pub fn timeout(
        &mut self,
        account: &str,
        channel: &str,
        remaining: Duration,
        now: Instant,
    ) -> bool {
        if remaining.is_zero() {
            return false;
        }
        let entry = self.entry(account, channel, now);
        let Some(deadline) = now.checked_add(remaining) else {
            return false;
        };
        let changed = !matches!(entry.restriction, Some(EligibilityRestriction::TimedOut(existing)) if existing == deadline);
        entry.restriction = Some(EligibilityRestriction::TimedOut(deadline));
        changed
    }

    pub fn ban(&mut self, account: &str, channel: &str, now: Instant) -> bool {
        let entry = self.entry(account, channel, now);
        if matches!(entry.restriction, Some(EligibilityRestriction::Banned)) {
            return false;
        }
        entry.restriction = Some(EligibilityRestriction::Banned);
        true
    }

    pub fn clear_restriction(&mut self, account: &str, channel: &str) -> bool {
        let Some(entry) = self
            .entries
            .get_mut(&(normalized_account(account), normalized_channel(channel)))
        else {
            return false;
        };
        if matches!(
            entry.restriction,
            Some(EligibilityRestriction::TimedOut(_) | EligibilityRestriction::Banned)
        ) {
            entry.restriction = None;
            true
        } else {
            false
        }
    }

    pub fn clear_slow_channel(&mut self, channel: &str) -> bool {
        let channel = normalized_channel(channel);
        let mut changed = false;
        for ((_, entry_channel), entry) in &mut self.entries {
            if entry_channel == &channel
                && matches!(entry.restriction, Some(EligibilityRestriction::Slow(_)))
            {
                entry.restriction = None;
                changed = true;
            }
        }
        changed
    }

    pub fn reset_exemptions(&mut self) -> bool {
        let mut changed = false;
        for entry in self.entries.values_mut() {
            changed |= std::mem::replace(&mut entry.exempt, false);
            changed |= std::mem::replace(&mut entry.exemption_known, false);
        }
        changed
    }

    pub fn status(
        &mut self,
        account: &str,
        channel: &str,
        now: Instant,
    ) -> Option<EligibilityStatus> {
        let entry = self
            .entries
            .get_mut(&(normalized_account(account), normalized_channel(channel)))?;
        entry.touched = now;
        let (block, next_change) = match entry.restriction.as_ref()? {
            EligibilityRestriction::Slow(deadline) => match deadline
                .checked_duration_since(now)
                .filter(|remaining| !remaining.is_zero())
            {
                Some(remaining) => (
                    EligibilityBlock::Slow(ceil_seconds(remaining)),
                    Some(next_second(remaining)),
                ),
                None => {
                    entry.restriction = None;
                    return None;
                }
            },
            EligibilityRestriction::TimedOut(deadline) => match deadline
                .checked_duration_since(now)
                .filter(|remaining| !remaining.is_zero())
            {
                Some(remaining) => (
                    EligibilityBlock::TimedOut(ceil_seconds(remaining)),
                    Some(next_second(remaining)),
                ),
                None => {
                    entry.restriction = None;
                    return None;
                }
            },
            EligibilityRestriction::Banned => (EligibilityBlock::Banned, None),
        };
        Some(EligibilityStatus { block, next_change })
    }

    fn entry(&mut self, account: &str, channel: &str, now: Instant) -> &mut EligibilityEntry {
        let key = (normalized_account(account), normalized_channel(channel));
        if !self.entries.contains_key(&key) {
            self.prune(now);
            self.entries.insert(
                key.clone(),
                EligibilityEntry {
                    exempt: false,
                    exemption_known: false,
                    restriction: None,
                    touched: now,
                },
            );
        }
        let entry = self.entries.get_mut(&key).expect("entry was inserted");
        entry.touched = now;
        entry
    }

    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            !matches!(entry.restriction, Some(EligibilityRestriction::Slow(deadline) | EligibilityRestriction::TimedOut(deadline)) if deadline <= now)
        });
        while self.entries.len() >= MAX_ELIGIBILITY_ENTRIES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

pub fn slow_wait_seconds(text: &str) -> Option<u64> {
    let text = text.to_ascii_lowercase();
    ["talk again in ", "try again in ", "send messages again in "]
        .iter()
        .filter_map(|phrase| {
            text.find(phrase)
                .map(|offset| &text[offset + phrase.len()..])
        })
        .find_map(|tail| {
            let digits = tail
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            let suffix = tail[digits.len()..].trim_start();
            let seconds = digits.parse::<u64>().ok()?;
            (seconds > 0
                && Duration::from_secs(seconds) <= MAX_SLOW_WAIT
                && suffix.strip_prefix("second").is_some_and(|tail| {
                    let tail = tail.strip_prefix('s').unwrap_or(tail);
                    !tail.starts_with(char::is_alphanumeric)
                }))
            .then_some(seconds)
        })
}

fn normalized_account(account: &str) -> String {
    account.trim().to_ascii_lowercase()
}

fn normalized_channel(channel: &str) -> String {
    echo_core::channel_identity::normalize_channel_key(channel.trim())
}

fn ceil_seconds(remaining: Duration) -> u64 {
    remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0)
}

fn next_second(remaining: Duration) -> Duration {
    if remaining.subsec_nanos() == 0 {
        Duration::from_secs(1)
    } else {
        Duration::from_nanos(remaining.subsec_nanos().into())
    }
}
