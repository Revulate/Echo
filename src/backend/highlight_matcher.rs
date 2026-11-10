//! Immutable compiled highlight rules for the IRC message ingress path.

use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use super::{HighlightRule, HighlightRuleEffects, HighlightRuleTarget, MatchedHighlightRule};

#[derive(Default)]
pub(super) struct CompiledHighlightRules {
    rules: Vec<CompiledHighlightRule>,
}

struct CompiledHighlightRule {
    target: HighlightRuleTarget,
    matcher: HighlightMatcher,
    pattern: String,
    color: Option<String>,
    show_in_mentions: bool,
    play_sound: bool,
    alert: bool,
    sound_path: Option<String>,
}

enum HighlightMatcher {
    Phrase(regex::Regex),
    UserExact {
        pattern: String,
        case_sensitive: bool,
    },
    UserRegex(regex::Regex),
    BadgeExact {
        candidates: Vec<BadgeCandidate>,
        case_sensitive: bool,
    },
    BadgeRegex(regex::Regex),
}

struct BadgeCandidate {
    name: String,
    value: Option<String>,
}

/// The published rule set is immutable. Incoming messages only load one Arc
/// and run prebuilt matchers; edits compile a replacement before publishing it.
static COMPILED_HIGHLIGHT_RULES: LazyLock<ArcSwap<CompiledHighlightRules>> =
    LazyLock::new(|| ArcSwap::from_pointee(CompiledHighlightRules::default()));
static COMPILED_HIGHLIGHT_RULES_PUBLISHED: AtomicBool = AtomicBool::new(false);

pub(super) fn publish(rules: &[HighlightRule]) {
    COMPILED_HIGHLIGHT_RULES.store(Arc::new(CompiledHighlightRules::from_rules(rules)));
    COMPILED_HIGHLIGHT_RULES_PUBLISHED.store(true, Ordering::Release);
}

fn current() -> arc_swap::Guard<Arc<CompiledHighlightRules>> {
    if !COMPILED_HIGHLIGHT_RULES_PUBLISHED.load(Ordering::Acquire) {
        LazyLock::force(&super::SETTINGS);
    }
    COMPILED_HIGHLIGHT_RULES.load()
}

impl CompiledHighlightRules {
    fn from_rules(rules: &[HighlightRule]) -> Self {
        let rules = rules
            .iter()
            .filter(|rule| rule.enabled)
            .filter_map(CompiledHighlightRule::from_rule)
            .collect();
        Self { rules }
    }

    pub(super) fn first_match(
        &self,
        message: &str,
        sender_login: &str,
        badges: &[String],
        targets: &[HighlightRuleTarget],
        is_self_sender: bool,
    ) -> Option<MatchedHighlightRule> {
        for target in targets {
            for rule in self.rules.iter().filter(|rule| rule.target == *target) {
                if let Some(badge) = rule.matches(message, sender_login, badges, is_self_sender) {
                    return Some(rule.matched_rule(sender_login, badge));
                }
            }
        }
        None
    }

    pub(super) fn effects(
        &self,
        message: &str,
        sender_login: &str,
        badges: &[String],
        targets: &[HighlightRuleTarget],
        is_self_sender: bool,
    ) -> Option<HighlightRuleEffects> {
        let mut effects = None;
        for target in targets {
            for rule in self.rules.iter().filter(|rule| rule.target == *target) {
                if rule
                    .matches(message, sender_login, badges, is_self_sender)
                    .is_none()
                {
                    continue;
                }
                let merged = effects.get_or_insert_with(HighlightRuleEffects::default);
                if !merged.play_sound && rule.play_sound {
                    merged.play_sound = true;
                    merged.sound_path = rule.sound_path.clone();
                }
                merged.alert |= rule.alert;
            }
        }
        effects
    }
}

impl CompiledHighlightRule {
    fn from_rule(rule: &HighlightRule) -> Option<Self> {
        let pattern = rule.normalized_pattern()?;
        let matcher = match rule.target {
            HighlightRuleTarget::Phrase => compile_phrase(pattern, rule)?,
            HighlightRuleTarget::User => compile_user(pattern, rule)?,
            HighlightRuleTarget::Badge => compile_badge(pattern, rule)?,
        };
        Some(Self {
            target: rule.target,
            matcher,
            pattern: rule.pattern.clone(),
            color: rule.color.clone(),
            show_in_mentions: rule.show_in_mentions,
            play_sound: rule.play_sound,
            alert: rule.alert,
            sound_path: rule.sound_path.clone(),
        })
    }

    fn matches(
        &self,
        message: &str,
        sender_login: &str,
        badges: &[String],
        is_self_sender: bool,
    ) -> Option<Option<String>> {
        match &self.matcher {
            HighlightMatcher::Phrase(regex) => {
                (!is_self_sender && regex.is_match(message)).then_some(None)
            }
            HighlightMatcher::UserExact {
                pattern,
                case_sensitive,
            } => {
                let matches = if *case_sensitive {
                    sender_login == pattern
                } else {
                    sender_login.eq_ignore_ascii_case(pattern)
                };
                matches.then_some(None)
            }
            HighlightMatcher::UserRegex(regex) => regex.is_match(sender_login).then_some(None),
            HighlightMatcher::BadgeExact {
                candidates,
                case_sensitive,
            } => candidates.iter().find_map(|candidate| {
                badges
                    .iter()
                    .find(|badge| badge_matches(candidate, badge, *case_sensitive))
                    .cloned()
                    .map(Some)
            }),
            HighlightMatcher::BadgeRegex(regex) => badges
                .iter()
                .find(|badge| regex.is_match(badge))
                .cloned()
                .map(Some),
        }
    }

    fn matched_rule(&self, sender_login: &str, badge: Option<String>) -> MatchedHighlightRule {
        let highlight_type = match self.target {
            HighlightRuleTarget::Phrase => crate::backend::models::HighlightType::CustomPhrase {
                pattern: self.pattern.clone(),
                color: self.color.clone(),
            },
            HighlightRuleTarget::User => crate::backend::models::HighlightType::CustomUser {
                username: sender_login.to_string(),
                color: self.color.clone(),
            },
            HighlightRuleTarget::Badge => crate::backend::models::HighlightType::CustomBadge {
                badge: badge.expect("badge matcher always returns a badge"),
                color: self.color.clone(),
            },
        };
        MatchedHighlightRule {
            highlight_type,
            show_in_mentions: self.show_in_mentions,
            play_sound: self.play_sound,
            alert: self.alert,
            sound_path: self.sound_path.clone(),
        }
    }
}

fn compile_regex(pattern: &str, rule: &HighlightRule) -> Option<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!rule.case_sensitive)
        .unicode(true)
        .build()
        .ok()
}

fn compile_phrase(pattern: &str, rule: &HighlightRule) -> Option<HighlightMatcher> {
    if rule.regex {
        return compile_regex(pattern, rule).map(HighlightMatcher::Phrase);
    }
    let boundary_pattern = format!(r"(?:\b|\s|^){}(?:\b|\s|$)", regex::escape(pattern));
    compile_regex(&boundary_pattern, rule).map(HighlightMatcher::Phrase)
}

fn compile_user(pattern: &str, rule: &HighlightRule) -> Option<HighlightMatcher> {
    if rule.regex {
        compile_regex(pattern, rule).map(HighlightMatcher::UserRegex)
    } else {
        Some(HighlightMatcher::UserExact {
            pattern: pattern.to_owned(),
            case_sensitive: rule.case_sensitive,
        })
    }
}

fn compile_badge(pattern: &str, rule: &HighlightRule) -> Option<HighlightMatcher> {
    if rule.regex {
        return compile_regex(pattern, rule).map(HighlightMatcher::BadgeRegex);
    }
    let candidates = pattern
        .split(',')
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(BadgeCandidate::parse)
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(HighlightMatcher::BadgeExact {
        candidates,
        case_sensitive: rule.case_sensitive,
    })
}

impl BadgeCandidate {
    fn parse(candidate: &str) -> Self {
        let (name, value) = split_badge_key_value(candidate);
        Self {
            name: name.to_owned(),
            value: value.map(ToOwned::to_owned),
        }
    }
}

fn badge_matches(candidate: &BadgeCandidate, badge: &str, case_sensitive: bool) -> bool {
    let (badge_name, badge_value) = split_badge_key_value(badge);
    let name_matches = if case_sensitive {
        badge_name == candidate.name
    } else {
        badge_name.eq_ignore_ascii_case(&candidate.name)
    };
    name_matches
        && candidate.value.as_deref().is_none_or(|candidate_value| {
            badge_value.is_some_and(|value| {
                if case_sensitive {
                    value == candidate_value
                } else {
                    value.eq_ignore_ascii_case(candidate_value)
                }
            })
        })
}

fn split_badge_key_value(badge: &str) -> (&str, Option<&str>) {
    badge
        .split_once('/')
        .or_else(|| badge.split_once(':'))
        .map(|(name, value)| (name, Some(value)))
        .unwrap_or((badge, None))
}

/// The published rule set, for a one-off evaluation that must not see a
/// concurrent edit halfway through.
pub(super) fn published() -> Arc<CompiledHighlightRules> {
    Arc::clone(&current())
}

/// Compile `rules` the way a publish does, without publishing them.
pub(super) fn compile(rules: &[HighlightRule]) -> Arc<CompiledHighlightRules> {
    Arc::new(CompiledHighlightRules::from_rules(rules))
}

/// Whether `rule` (enabled or not) would record `origin` on a row it
/// highlights: the same pattern for a phrase rule, or a matcher that accepts
/// the recorded sender or badge for a user or badge rule.
pub(super) fn rule_produces_origin(
    rule: &HighlightRule,
    origin: echo_core::highlight_explanation::HighlightRuleOrigin<'_>,
) -> bool {
    use echo_core::highlight_explanation::HighlightRuleKind;

    let target = match origin.kind {
        HighlightRuleKind::Phrase => HighlightRuleTarget::Phrase,
        HighlightRuleKind::User => HighlightRuleTarget::User,
        HighlightRuleKind::Badge => HighlightRuleTarget::Badge,
    };
    if rule.target != target {
        return false;
    }
    match origin.kind {
        HighlightRuleKind::Phrase => rule.pattern == origin.value,
        HighlightRuleKind::User => CompiledHighlightRule::from_rule(rule)
            .is_some_and(|compiled| compiled.matches("", origin.value, &[], false).is_some()),
        HighlightRuleKind::Badge => {
            let badges = [origin.value.to_owned()];
            CompiledHighlightRule::from_rule(rule)
                .is_some_and(|compiled| compiled.matches("", "", &badges, false).is_some())
        }
    }
}

pub(super) fn match_rule_for_targets(
    message: &str,
    sender_login: &str,
    badges: &[String],
    targets: &[HighlightRuleTarget],
    current_user: &str,
) -> Option<MatchedHighlightRule> {
    let is_self_sender =
        !current_user.is_empty() && sender_login.eq_ignore_ascii_case(current_user);
    current().first_match(message, sender_login, badges, targets, is_self_sender)
}

pub(super) fn match_effects_for_targets(
    message: &str,
    sender_login: &str,
    badges: &[String],
    targets: &[HighlightRuleTarget],
    current_user: &str,
) -> Option<HighlightRuleEffects> {
    let is_self_sender =
        !current_user.is_empty() && sender_login.eq_ignore_ascii_case(current_user);
    current().effects(message, sender_login, badges, targets, is_self_sender)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn rule(target: HighlightRuleTarget, pattern: &str) -> HighlightRule {
        HighlightRule {
            id: pattern.to_owned(),
            target,
            pattern: pattern.to_owned(),
            regex: false,
            case_sensitive: false,
            show_in_mentions: true,
            play_sound: false,
            alert: false,
            sound_path: None,
            enabled: true,
            color: None,
        }
    }

    #[test]
    fn preserves_target_order_and_phrase_boundary_case_semantics() {
        let mut invalid = rule(HighlightRuleTarget::Phrase, "[");
        invalid.regex = true;
        let phrase = rule(HighlightRuleTarget::Phrase, "needle");
        let user = rule(HighlightRuleTarget::User, "sender");
        let compiled = CompiledHighlightRules::from_rules(&[invalid, phrase, user]);

        let matched = compiled
            .first_match(
                "NEEDLE",
                "sender",
                &[],
                &[HighlightRuleTarget::User, HighlightRuleTarget::Phrase],
                false,
            )
            .expect("user rule matches first target");
        assert!(matches!(
            matched.highlight_type,
            crate::backend::models::HighlightType::CustomUser { .. }
        ));
        assert!(
            compiled
                .first_match(
                    "NEEDLEX",
                    "other",
                    &[],
                    &[HighlightRuleTarget::Phrase],
                    false,
                )
                .is_none()
        );
        assert!(
            compiled
                .first_match(
                    "NEEDLE",
                    "other",
                    &[],
                    &[HighlightRuleTarget::Phrase],
                    false,
                )
                .is_some()
        );
    }

    #[test]
    fn preserves_badge_text_and_merges_effects_in_rule_order() {
        let badge = rule(HighlightRuleTarget::Badge, "subscriber/12, vip");
        let mut alert = rule(HighlightRuleTarget::Phrase, "alert");
        alert.alert = true;
        let mut sound = rule(HighlightRuleTarget::Phrase, "alert");
        sound.play_sound = true;
        sound.sound_path = Some("ding.wav".to_owned());
        let compiled = CompiledHighlightRules::from_rules(&[badge, alert, sound]);

        let matched = compiled
            .first_match(
                "ignored",
                "sender",
                &["subscriber/12".to_owned()],
                &[HighlightRuleTarget::Badge],
                false,
            )
            .expect("badge rule matches");
        assert!(matches!(
            matched.highlight_type,
            crate::backend::models::HighlightType::CustomBadge { ref badge, .. }
                if badge == "subscriber/12"
        ));

        let effects = compiled
            .effects(
                "ALERT",
                "sender",
                &[],
                &[HighlightRuleTarget::Phrase],
                false,
            )
            .expect("phrase rules match");
        assert!(effects.alert);
        assert!(effects.play_sound);
        assert_eq!(effects.sound_path.as_deref(), Some("ding.wav"));
    }

    #[test]
    fn rule_origin_matches_the_recorded_pattern_sender_or_badge() {
        use echo_core::highlight_explanation::{HighlightRuleKind, HighlightRuleOrigin};

        let origin = |kind, value| HighlightRuleOrigin {
            kind,
            value,
            color: None,
        };
        let phrase = rule(HighlightRuleTarget::Phrase, "pog");
        assert!(rule_produces_origin(
            &phrase,
            origin(HighlightRuleKind::Phrase, "pog")
        ));
        assert!(!rule_produces_origin(
            &phrase,
            origin(HighlightRuleKind::Phrase, "POG")
        ));
        assert!(!rule_produces_origin(
            &phrase,
            origin(HighlightRuleKind::User, "pog")
        ));

        let mut user = rule(HighlightRuleTarget::User, "^x.*");
        user.regex = true;
        assert!(rule_produces_origin(
            &user,
            origin(HighlightRuleKind::User, "xqc")
        ));
        assert!(!rule_produces_origin(
            &user,
            origin(HighlightRuleKind::User, "forsen")
        ));

        let badge = rule(HighlightRuleTarget::Badge, "subscriber/12, vip");
        assert!(rule_produces_origin(
            &badge,
            origin(HighlightRuleKind::Badge, "vip/1")
        ));
        assert!(!rule_produces_origin(
            &badge,
            origin(HighlightRuleKind::Badge, "subscriber/3")
        ));
    }
}
