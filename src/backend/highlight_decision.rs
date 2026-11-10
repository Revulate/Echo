//! The highlight decision for an incoming chat message.
//!
//! IRC ingress (`process_privmsg_message`) and the Highlights tester both run
//! [`decide_privmsg_highlight`], [`fold_privmsg_filter_results`] and
//! [`privmsg_shows_in_mentions`], so what the tester reports is what live chat
//! does. Kept out of `highlight_priority.rs`, which a contract tool compiles
//! against stubs.

use super::filters::{FilterAction, FilterTestResult};
use super::highlight_priority::{PrivmsgHighlightInput, select_privmsg_highlight};
use super::models::HighlightType;
use super::settings::{HighlightRuleEffects, MatchedHighlightRule};
use super::sound_events::sound_for_highlight_type;
use super::sounds::SoundEventType;

/// What the enabled filters decided for a message.
#[derive(Debug, Default)]
pub struct PrivmsgFilterOutcome {
    pub hide: bool,
    pub highlighted: bool,
    /// Text a highlighting filter matched, when it recorded any.
    pub matched_text: Option<String>,
}

/// Fold filter results in rule order: Hide and Allow stop the scan, Allow
/// also clears an earlier Highlight.
pub fn fold_privmsg_filter_results(results: &[FilterTestResult]) -> PrivmsgFilterOutcome {
    let mut outcome = PrivmsgFilterOutcome::default();
    for result in results {
        if result.matched {
            match result.action {
                FilterAction::Hide => {
                    outcome.hide = true;
                    break;
                }
                FilterAction::Highlight => {
                    outcome.highlighted = true;
                    if outcome.matched_text.is_none() {
                        outcome.matched_text = result
                            .matched_text
                            .clone()
                            .filter(|text| !text.trim().is_empty());
                    }
                }
                FilterAction::Allow => {
                    outcome.hide = false;
                    outcome.highlighted = false;
                    outcome.matched_text = None;
                    break;
                }
                FilterAction::Replace => {}
            }
        }
    }
    outcome
}

/// A mention: the message names the signed-in account or replies to one of
/// its messages, and the sender is not excluded from highlights.
pub fn is_mention_highlight(blacklisted_sender: bool, account_name_mention: bool) -> bool {
    !blacklisted_sender && account_name_mention
}

/// Facts about a message gathered before its highlight is decided.
pub struct PrivmsgHighlightSignals<'a> {
    /// The sender is on the highlight exclusion list.
    pub blacklisted_sender: bool,
    /// Names the signed-in account, or replies to one of its messages.
    pub account_name_mention: bool,
    /// Sent by the signed-in account.
    pub own_sender: bool,
    /// The signed-in login, when the message replies in a thread it posted in.
    pub participated_thread_username: Option<&'a str>,
    pub filter_matched_text: Option<String>,
    pub filter_highlighted: bool,
    pub cheer: bool,
    pub first_message: bool,
    pub returning_chatter: bool,
    /// Elevated or Hype Chat message.
    pub elevated: bool,
    /// Similar to a recent message while similar messages are dimmed.
    pub similar_dim_candidate: bool,
}

/// The highlight, sound and alert choice for one message.
#[derive(Debug)]
pub struct PrivmsgHighlightDecision {
    /// A mention (see [`is_mention_highlight`]).
    pub highlighted: bool,
    pub highlight_type: Option<HighlightType>,
    /// Dimmed as a similar message instead of highlighted.
    pub is_similar_dim: bool,
    /// A custom highlight rule chose `highlight_type`.
    pub rule_chose_highlight: bool,
    /// Effects merged across every matching custom rule, looked up only when
    /// a custom rule chose the highlight.
    pub rule_effects: Option<HighlightRuleEffects>,
    /// Built-in sound for `highlight_type`, unless suppressed.
    pub highlight_sound: Option<SoundEventType>,
}

/// The sound live chat plays for a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivmsgHighlightSound<'a> {
    Event(SoundEventType),
    /// A rule's sound file, falling back to the Phrase sound when it is missing.
    RuleFile(&'a str),
}

/// The desktop alert live chat raises for a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivmsgHighlightAlert {
    None,
    /// Mention notification; flashes the taskbar when "Flash window on
    /// mention" is on.
    Mention,
    /// Custom-rule notification; always flashes the taskbar.
    Rule,
}

impl PrivmsgHighlightDecision {
    /// A matching custom rule has Alert on.
    pub fn rule_alert(&self) -> bool {
        self.rule_effects
            .as_ref()
            .map(|effects| effects.alert)
            .unwrap_or(false)
    }

    /// The sound file of the first matching rule that plays a sound.
    pub fn rule_sound_path(&self) -> Option<&str> {
        self.rule_effects
            .as_ref()
            .filter(|effects| effects.play_sound)
            .and_then(|effects| effects.sound_path.as_deref())
    }

    pub fn sound(&self) -> Option<PrivmsgHighlightSound<'_>> {
        if self.highlighted {
            Some(PrivmsgHighlightSound::Event(SoundEventType::Mention))
        } else if let Some(path) = self.rule_sound_path() {
            Some(PrivmsgHighlightSound::RuleFile(path))
        } else {
            self.highlight_sound.map(PrivmsgHighlightSound::Event)
        }
    }

    pub fn alert(&self) -> PrivmsgHighlightAlert {
        if self.highlighted {
            PrivmsgHighlightAlert::Mention
        } else if self.rule_alert() {
            PrivmsgHighlightAlert::Rule
        } else {
            PrivmsgHighlightAlert::None
        }
    }
}

/// Pick the highlight, then the self-message and elevated overrides, then the
/// sound. `rule_effects` runs only when a custom rule chose the highlight.
pub fn decide_privmsg_highlight(
    signals: PrivmsgHighlightSignals<'_>,
    configured_highlight: Option<&MatchedHighlightRule>,
    configured_badge_highlight: Option<&MatchedHighlightRule>,
    rule_effects: impl FnOnce() -> Option<HighlightRuleEffects>,
) -> PrivmsgHighlightDecision {
    let highlighted =
        is_mention_highlight(signals.blacklisted_sender, signals.account_name_mention);
    let mut highlight_type = if signals.blacklisted_sender {
        None
    } else {
        select_privmsg_highlight(PrivmsgHighlightInput {
            account_name_mention: signals.account_name_mention,
            configured_highlight: configured_highlight
                .map(|matched| matched.highlight_type.clone()),
            configured_badge_highlight: configured_badge_highlight
                .map(|matched| matched.highlight_type.clone()),
            reply_thread_username: signals.participated_thread_username.map(str::to_owned),
            filter_matched_text: signals.filter_matched_text,
            filter_highlighted: signals.filter_highlighted,
            cheer: signals.cheer,
            first_message: signals.first_message,
            returning_chatter: signals.returning_chatter,
        })
    };
    if highlight_type.is_none()
        && signals.own_sender
        && super::settings::get_enable_self_message_highlight()
    {
        highlight_type = Some(HighlightType::SelfMessage);
    }
    if signals.elevated && super::settings::get_enable_elevated_highlight() {
        highlight_type = Some(HighlightType::Elevated);
    }
    let is_similar_dim =
        !signals.blacklisted_sender && highlight_type.is_none() && signals.similar_dim_candidate;
    let rule_chose_highlight = configured_highlight
        .into_iter()
        .chain(configured_badge_highlight)
        .any(|matched| highlight_type.as_ref() == Some(&matched.highlight_type));
    let rule_effects = if rule_chose_highlight {
        rule_effects()
    } else {
        None
    };
    let rule_plays_sound = rule_effects
        .as_ref()
        .map(|effects| effects.play_sound)
        .unwrap_or(false);
    let highlight_sound = if signals.blacklisted_sender
        || is_similar_dim
        || (rule_chose_highlight && !rule_plays_sound)
    {
        None
    } else {
        sound_for_highlight_type(highlight_type.as_ref())
    };
    PrivmsgHighlightDecision {
        highlighted,
        highlight_type,
        is_similar_dim,
        rule_chose_highlight,
        rule_effects,
        highlight_sound,
    }
}

/// Whether the message joins Activity Mentions: every mention, participated
/// reply threads when that setting is on, and custom rules with Show in
/// Mentions when they chose the row's highlight.
pub fn privmsg_shows_in_mentions(
    highlighted: bool,
    highlight_type: Option<&HighlightType>,
    configured_highlight: Option<&MatchedHighlightRule>,
    configured_badge_highlight: Option<&MatchedHighlightRule>,
) -> bool {
    let show_thread_reply_in_mentions = matches!(
        highlight_type,
        Some(HighlightType::ParticipatedReply { .. })
    ) && super::settings::get_thread_highlight_show_in_mentions(
    );
    let show_configured_highlight_in_mentions = configured_highlight
        .into_iter()
        .chain(configured_badge_highlight)
        .any(|matched| matched.show_in_mentions && highlight_type == Some(&matched.highlight_type));
    highlighted || show_thread_reply_in_mentions || show_configured_highlight_in_mentions
}

/// A sample message for the Highlights tester. It is evaluated only; it never
/// reaches chat, history, Activity or logs.
#[derive(Clone, Debug, Default)]
pub struct HighlightSample {
    pub message: String,
    pub sender_login: String,
    /// Twitch badge keys such as `moderator/1`; a bare name means version 1.
    pub badges: Vec<String>,
    pub channel: String,
    /// Evaluate as the signed-in account's own message.
    pub sent_by_me: bool,
    /// Rules to match instead of the saved ones (the saved rules with the
    /// rule editor's draft in place).
    pub rules: Option<Vec<super::settings::HighlightRule>>,
}

/// A sound the tester can play on request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HighlightTestSound {
    Event(SoundEventType),
    RuleFile(String),
}

/// What live chat would do with a [`HighlightSample`].
#[derive(Clone, Debug)]
pub struct HighlightTestOutcome {
    /// Login the sample was evaluated as.
    pub sender_login: String,
    pub hidden_by_filter: bool,
    pub highlight_type: Option<HighlightType>,
    pub in_mentions: bool,
    pub sound: Option<HighlightTestSound>,
    pub sound_summary: String,
    pub sound_note: Option<String>,
    pub alert_summary: String,
    pub alert_note: Option<String>,
    /// Caveats about the sample itself.
    pub notes: Vec<String>,
}

/// Evaluate `sample` through the same steps as `process_privmsg_message`,
/// without touching any chat state. Replies, cheers, first-message and
/// returning-chatter tags, Elevated/Hype Chat, similar-message hiding or
/// dimming, blocked senders and sub-month filter fields are not part of a
/// sample. Runs on the UI thread, so it never probes a network path and reads
/// streamer mode from cached detection.
pub fn test_highlight_sample(sample: &HighlightSample) -> HighlightTestOutcome {
    use super::settings::{HighlightRuleSet, HighlightRuleTarget};

    let username = super::settings::get_username_lowercase();
    let mut notes = Vec::new();
    let typed_sender = sample
        .sender_login
        .trim()
        .trim_start_matches('@')
        .to_lowercase();
    let sender_login = if sample.sent_by_me && !username.is_empty() {
        username.clone()
    } else {
        typed_sender
    };
    if sample.sent_by_me && username.is_empty() {
        notes.push("Sign in to test your own messages.".to_string());
    }
    let channel = sample.channel.trim().trim_start_matches('#').to_lowercase();

    let (clean_text, is_action) = super::irc::format::normalize_message_text(&sample.message);
    let contains_account_name = super::irc::contains_account_mention(&clean_text, &username);
    let own_sender = !username.is_empty() && sender_login.eq_ignore_ascii_case(&username);
    let blacklisted_sender = super::settings::is_highlight_blacklisted_user(&sender_login);
    let contains_account_name = contains_account_name && !own_sender;
    let highlighted = is_mention_highlight(blacklisted_sender, contains_account_name);

    let content = super::filters_replace::apply_ignore_replacements(
        &clean_text,
        &super::filters_replace::TwitchEmotePositionMap::default(),
    )
    .map_or(clean_text, |(text, _)| text);
    let badges: Vec<String> = sample
        .badges
        .iter()
        .filter_map(|badge| sample_badge_key(badge))
        .collect();
    let badge_names: Vec<&str> = badges
        .iter()
        .map(|badge| {
            badge
                .split_once('/')
                .map_or(badge.as_str(), |(name, _)| name)
        })
        .collect();

    let filter_ctx = super::filters::TestContext {
        content: content.clone(),
        author: sender_login.clone(),
        author_login: sender_login.clone(),
        badges: super::irc::badge_names_for_filter(badge_names.iter().copied()),
        channel_name: channel.clone(),
        is_sub: badge_names.contains(&"subscriber"),
        is_action,
        is_highlighted: highlighted,
        channel_watching: true,
        channel_live: super::bridge::get_cached_channel_live_status(&channel),
        ..Default::default()
    };
    let filter_results = if super::settings::get_exclude_user_messages_from_filter() && own_sender {
        Vec::new()
    } else {
        super::filters::FILTER_STORE.test_message(&filter_ctx)
    };
    let filters = fold_privmsg_filter_results(&filter_results);
    if filters.hide {
        return HighlightTestOutcome {
            sender_login,
            hidden_by_filter: true,
            highlight_type: None,
            in_mentions: false,
            sound: None,
            sound_summary: "No sound".to_string(),
            sound_note: Some("A filter hides this message.".to_string()),
            alert_summary: "No desktop alert".to_string(),
            alert_note: Some("A filter hides this message.".to_string()),
            notes,
        };
    }

    let rules = match sample.rules.as_deref() {
        Some(rules) => HighlightRuleSet::from_rules(rules),
        None => HighlightRuleSet::published(),
    };
    let (configured, configured_badge) = if blacklisted_sender {
        (None, None)
    } else {
        (
            rules.match_for_targets(
                &content,
                &sender_login,
                &badges,
                &[HighlightRuleTarget::Phrase, HighlightRuleTarget::User],
                own_sender,
            ),
            rules.match_for_targets(
                &content,
                &sender_login,
                &badges,
                &[HighlightRuleTarget::Badge],
                own_sender,
            ),
        )
    };
    let decision = decide_privmsg_highlight(
        PrivmsgHighlightSignals {
            blacklisted_sender,
            account_name_mention: contains_account_name,
            own_sender,
            participated_thread_username: None,
            filter_matched_text: filters.matched_text,
            filter_highlighted: filters.highlighted,
            cheer: false,
            first_message: false,
            returning_chatter: false,
            elevated: false,
            similar_dim_candidate: false,
        },
        configured.as_ref(),
        configured_badge.as_ref(),
        || {
            rules.effects_for_targets(
                &content,
                &sender_login,
                &badges,
                &[
                    HighlightRuleTarget::Phrase,
                    HighlightRuleTarget::User,
                    HighlightRuleTarget::Badge,
                ],
                own_sender,
            )
        },
    );
    let in_mentions = privmsg_shows_in_mentions(
        decision.highlighted,
        decision.highlight_type.as_ref(),
        configured.as_ref(),
        configured_badge.as_ref(),
    );

    let (sound, sound_summary, sound_note) =
        describe_sound(&decision, blacklisted_sender, &channel);
    let (alert_summary, alert_note) = describe_alert(&decision, blacklisted_sender, &channel);
    HighlightTestOutcome {
        sender_login,
        hidden_by_filter: false,
        highlight_type: decision.highlight_type.clone(),
        in_mentions,
        sound,
        sound_summary,
        sound_note,
        alert_summary,
        alert_note,
        notes,
    }
}

/// A typed badge as the `name/version` key IRC ingress builds: lowercase,
/// `:` accepted as the separator, and version 1 when none is given.
fn sample_badge_key(badge: &str) -> Option<String> {
    let badge = badge.trim().to_lowercase();
    let (name, version) = badge
        .split_once('/')
        .or_else(|| badge.split_once(':'))
        .map_or((badge.as_str(), ""), |(name, version)| {
            (name.trim(), version.trim())
        });
    let version = if version.is_empty() { "1" } else { version };
    (!name.is_empty()).then(|| format!("{name}/{version}"))
}

const EXCLUDED_SENDER: &str = "The sender is on your highlight exclusion list.";

fn describe_sound(
    decision: &PrivmsgHighlightDecision,
    blacklisted_sender: bool,
    channel: &str,
) -> (Option<HighlightTestSound>, String, Option<String>) {
    let (sound, summary, event, mut notes) = match decision.sound() {
        Some(PrivmsgHighlightSound::Event(event)) => (
            HighlightTestSound::Event(event),
            format!("{} sound", event.display_name()),
            event,
            Vec::new(),
        ),
        Some(PrivmsgHighlightSound::RuleFile(path)) => {
            let trimmed = path.trim().trim_matches('"');
            let file = std::path::Path::new(trimmed)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| trimmed.to_string());
            let mut notes = Vec::new();
            // Probing a network share authenticates to its host, so it is
            // never touched from the tester.
            if super::settings::is_remote_or_device_path(trimmed) {
                notes.push(format!(
                    "The rule's sound file is on a network path, not checked. Play sound plays the {} sound instead.",
                    SoundEventType::Phrase.display_name()
                ));
            } else if !std::path::Path::new(trimmed).exists() {
                notes.push(format!(
                    "The rule's sound file is missing, so the {} sound plays instead.",
                    SoundEventType::Phrase.display_name()
                ));
            }
            (
                HighlightTestSound::RuleFile(path.to_string()),
                format!("Your rule's sound: {file}"),
                SoundEventType::Phrase,
                notes,
            )
        }
        None => {
            let reason = if blacklisted_sender {
                EXCLUDED_SENDER
            } else {
                silent_reason(decision)
            };
            return (None, "No sound".to_string(), Some(reason.to_string()));
        }
    };
    if let Some(blocked) = sound_blocked_reason(event, channel) {
        notes.push(blocked);
    }
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    (Some(sound), summary, note)
}

fn silent_reason(decision: &PrivmsgHighlightDecision) -> &'static str {
    if decision.highlight_type.is_none() {
        "The message isn't highlighted."
    } else if decision.rule_chose_highlight
        && !decision
            .rule_effects
            .as_ref()
            .is_some_and(|effects| effects.play_sound)
    {
        "The matching rule has Play Sound off."
    } else if matches!(
        decision.highlight_type,
        Some(HighlightType::ParticipatedReply { .. })
    ) {
        "\"Play reply-thread sound\" is off."
    } else {
        "This kind of highlight doesn't play a sound."
    }
}

/// Why the sound store would not play `event` right now, in the order its
/// play paths check.
fn sound_blocked_reason(event: SoundEventType, channel: &str) -> Option<String> {
    let store = &super::sounds::SOUND_STORE;
    if store.is_channel_muted(channel) {
        return Some(format!("Muted: sounds are off for #{channel}."));
    }
    if event == SoundEventType::Mention && super::streamer_mode::should_mute_mentions_cached() {
        return Some("Muted: streamer mode is silencing mention sounds right now.".to_string());
    }
    let settings = store.settings();
    if !settings.global_enabled {
        return Some("Muted: sounds are turned off in Sounds.".to_string());
    }
    let enabled = settings
        .events
        .get(&event)
        .map(|event| event.enabled)
        .unwrap_or_else(|| event.default_settings().enabled);
    (!enabled).then(|| {
        format!(
            "Muted: the {} sound is turned off in Sounds.",
            event.display_name()
        )
    })
}

fn describe_alert(
    decision: &PrivmsgHighlightDecision,
    blacklisted_sender: bool,
    channel: &str,
) -> (String, Option<String>) {
    let (event, flashes) = match decision.alert() {
        PrivmsgHighlightAlert::Mention => (
            SoundEventType::Mention,
            super::settings::get_flash_on_mention(),
        ),
        PrivmsgHighlightAlert::Rule => (SoundEventType::Phrase, true),
        PrivmsgHighlightAlert::None => {
            let reason = if blacklisted_sender {
                EXCLUDED_SENDER
            } else if decision.rule_effects.is_some() {
                "The matching rule has Alert off."
            } else {
                "Only mentions and rules with Alert on send one."
            };
            return ("No desktop alert".to_string(), Some(reason.to_string()));
        }
    };
    let suppressed = super::streamer_mode::should_suppress_notifications_cached();
    let blocked = if !super::sounds::SOUND_STORE
        .settings()
        .desktop_notifications_enabled
    {
        Some("Desktop notifications are turned off in Notifications.".to_string())
    } else if !super::notifications::desktop_notifications_enabled_for_event(Some(event)) {
        Some(format!(
            "The {} sound is turned off, which also turns off its desktop alert.",
            event.display_name()
        ))
    } else if super::sounds::SOUND_STORE.is_channel_desktop_muted(channel) {
        Some(format!("Desktop alerts are muted for #{channel}."))
    } else {
        None
    };
    let mut notes: Vec<String> = Vec::new();
    let summary = match blocked {
        Some(blocked) => {
            notes.push(blocked);
            "No desktop alert"
        }
        None => {
            notes.push("Only while Echo isn't focused, and never in streamer mode.".to_string());
            if suppressed {
                notes.push("Streamer mode is suppressing it right now.".to_string());
            }
            "Desktop alert"
        }
    };
    // `flash_taskbar_for_attention` returns early under the same suppression.
    if flashes && !suppressed {
        notes.push("The taskbar flashes while Echo isn't focused.".to_string());
    }
    (summary.to_string(), Some(notes.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn filter(matched: bool, action: FilterAction, text: Option<&str>) -> FilterTestResult {
        FilterTestResult {
            matched,
            action,
            matched_text: text.map(str::to_string),
        }
    }

    fn decision(
        highlighted: bool,
        rule_effects: Option<HighlightRuleEffects>,
        highlight_sound: Option<SoundEventType>,
    ) -> PrivmsgHighlightDecision {
        PrivmsgHighlightDecision {
            highlighted,
            highlight_type: None,
            is_similar_dim: false,
            rule_chose_highlight: rule_effects.is_some(),
            rule_effects,
            highlight_sound,
        }
    }

    #[test]
    fn filter_results_fold_in_rule_order() {
        let highlighted = fold_privmsg_filter_results(&[
            filter(false, FilterAction::Hide, None),
            filter(true, FilterAction::Highlight, Some(" ")),
            filter(true, FilterAction::Highlight, Some("pog")),
        ]);
        assert!(!highlighted.hide);
        assert!(highlighted.highlighted);
        assert_eq!(highlighted.matched_text.as_deref(), Some("pog"));

        let allowed = fold_privmsg_filter_results(&[
            filter(true, FilterAction::Highlight, Some("pog")),
            filter(true, FilterAction::Allow, None),
            filter(true, FilterAction::Hide, None),
        ]);
        assert!(!allowed.hide);
        assert!(!allowed.highlighted);
        assert_eq!(allowed.matched_text, None);

        let hidden = fold_privmsg_filter_results(&[
            filter(true, FilterAction::Hide, None),
            filter(true, FilterAction::Allow, None),
        ]);
        assert!(hidden.hide);
    }

    #[test]
    fn sample_badges_use_the_ingress_key_format() {
        assert_eq!(
            sample_badge_key(" Subscriber:12 ").as_deref(),
            Some("subscriber/12")
        );
        assert_eq!(
            sample_badge_key("moderator/1").as_deref(),
            Some("moderator/1")
        );
        assert_eq!(sample_badge_key("VIP").as_deref(), Some("vip/1"));
        assert_eq!(sample_badge_key("vip/").as_deref(), Some("vip/1"));
        assert_eq!(sample_badge_key("  "), None);
        assert_eq!(sample_badge_key("/3"), None);
    }

    #[test]
    fn excluded_senders_are_never_mentions() {
        assert!(is_mention_highlight(false, true));
        assert!(!is_mention_highlight(true, true));
        assert!(!is_mention_highlight(false, false));
    }

    #[test]
    fn mentions_use_the_mention_sound_and_alert_over_rule_effects() {
        let effects = HighlightRuleEffects {
            play_sound: true,
            alert: true,
            sound_path: Some("ding.wav".to_string()),
        };
        let mention = decision(true, Some(effects.clone()), None);
        assert_eq!(
            mention.sound(),
            Some(PrivmsgHighlightSound::Event(SoundEventType::Mention))
        );
        assert_eq!(mention.alert(), PrivmsgHighlightAlert::Mention);

        let rule = decision(false, Some(effects), Some(SoundEventType::Phrase));
        assert_eq!(
            rule.sound(),
            Some(PrivmsgHighlightSound::RuleFile("ding.wav"))
        );
        assert_eq!(rule.alert(), PrivmsgHighlightAlert::Rule);
    }

    #[test]
    fn rule_sound_without_a_file_uses_the_highlight_sound() {
        let effects = HighlightRuleEffects {
            play_sound: true,
            alert: false,
            sound_path: None,
        };
        let rule = decision(false, Some(effects), Some(SoundEventType::Phrase));
        assert_eq!(
            rule.sound(),
            Some(PrivmsgHighlightSound::Event(SoundEventType::Phrase))
        );
        assert_eq!(rule.alert(), PrivmsgHighlightAlert::None);
        assert_eq!(decision(false, None, None).sound(), None);
    }
}
