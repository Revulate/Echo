//! GPUI-free per-emote display rules.
//!
//! A rule names one emote by provider and id and turns on either or both of two
//! independent presentation flags: show the emote's code as plain text, or show
//! the first frame of an animated emote. A rule applies everywhere or only in
//! one channel. The persisted list lives in the application settings; layout
//! reads a [`CompiledEmoteDisplayRules`] snapshot built from it, so the per-emote
//! check is one borrowed-key hash probe with no allocation.

use crate::emote_preferences::EmoteRef;
use echo_emote_core::model::EmoteProvider;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Where a rule applies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmoteRuleScope {
    #[default]
    Everywhere,
    /// Lowercase Twitch login, without `#`.
    Channel(String),
}

impl EmoteRuleScope {
    /// A channel scope for `channel`, normalized to a lowercase login.
    pub fn channel(channel: &str) -> Self {
        Self::Channel(normalize_channel(channel))
    }

    fn applies_to(&self, channel: &str) -> bool {
        match self {
            Self::Everywhere => true,
            Self::Channel(login) => channel_matches(login, channel),
        }
    }

    /// Scope equality after channel normalization, so `#XQC` and `xqc` name
    /// the same rule.
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Everywhere, Self::Everywhere) => true,
            (Self::Channel(login), Self::Channel(other)) => {
                channel_matches(&normalize_channel(login), other.trim())
            }
            _ => false,
        }
    }
}

/// One persisted rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmoteDisplayRule {
    /// Provider key and id identify the emote; the name is kept for display.
    pub emote: EmoteRef,
    #[serde(default)]
    pub scope: EmoteRuleScope,
    #[serde(default)]
    pub show_as_text: bool,
    #[serde(default)]
    pub still_image: bool,
}

impl EmoteDisplayRule {
    fn targets(&self, emote: &EmoteRef, scope: &EmoteRuleScope) -> bool {
        same_emote(&self.emote, emote) && self.scope.same_as(scope)
    }

    fn flags(&self) -> EmoteDisplayFlags {
        EmoteDisplayFlags {
            show_as_text: self.show_as_text,
            still_image: self.still_image,
        }
    }
}

/// The effective presentation of one emote occurrence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmoteDisplayFlags {
    pub show_as_text: bool,
    pub still_image: bool,
}

impl EmoteDisplayFlags {
    pub const fn is_none(self) -> bool {
        !self.show_as_text && !self.still_image
    }

    pub const fn get(self, flag: EmoteDisplayFlag) -> bool {
        match flag {
            EmoteDisplayFlag::ShowAsText => self.show_as_text,
            EmoteDisplayFlag::StillImage => self.still_image,
        }
    }

    const fn union(self, other: Self) -> Self {
        Self {
            show_as_text: self.show_as_text || other.show_as_text,
            still_image: self.still_image || other.still_image,
        }
    }
}

/// One of the two independent rule flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmoteDisplayFlag {
    ShowAsText,
    StillImage,
}

/// Set one flag of the rule for `emote` at `scope`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmoteRuleChange {
    pub emote: EmoteRef,
    pub scope: EmoteRuleScope,
    pub flag: EmoteDisplayFlag,
    pub value: bool,
}

/// Persisted provider key, matching the emote preference keys.
pub const fn provider_key(provider: EmoteProvider) -> &'static str {
    match provider {
        EmoteProvider::Twitch => "Twitch",
        EmoteProvider::SevenTV => "SevenTV",
        EmoteProvider::BTTV => "BTTV",
        EmoteProvider::FFZ => "FFZ",
        EmoteProvider::Emoji => "Emoji",
    }
}

/// The provider a persisted key names. Emoji have no emote identity, so they
/// never take rules; unknown keys are ignored.
pub fn provider_from_key(key: &str) -> Option<EmoteProvider> {
    match key {
        "Twitch" => Some(EmoteProvider::Twitch),
        "SevenTV" => Some(EmoteProvider::SevenTV),
        "BTTV" => Some(EmoteProvider::BTTV),
        "FFZ" => Some(EmoteProvider::FFZ),
        _ => None,
    }
}

/// Lowercase login without `#` or surrounding whitespace.
pub fn normalize_channel(channel: &str) -> String {
    crate::channel_identity::normalize_channel_key(channel.trim())
}

#[inline]
fn channel_matches(login: &str, channel: &str) -> bool {
    channel.trim_start_matches('#').eq_ignore_ascii_case(login)
}

#[inline]
fn same_emote(a: &EmoteRef, b: &EmoteRef) -> bool {
    a.id == b.id && a.provider == b.provider
}

#[derive(Default)]
struct CompiledEmoteRule {
    everywhere: EmoteDisplayFlags,
    channels: SmallVec<[(Box<str>, EmoteDisplayFlags); 1]>,
}

const PROVIDER_SLOTS: usize = 4;

#[inline]
const fn provider_slot(provider: EmoteProvider) -> Option<usize> {
    match provider {
        EmoteProvider::Twitch => Some(0),
        EmoteProvider::SevenTV => Some(1),
        EmoteProvider::BTTV => Some(2),
        EmoteProvider::FFZ => Some(3),
        EmoteProvider::Emoji => None,
    }
}

/// Lookup snapshot of the active rules, keyed by provider and emote id.
#[derive(Default)]
pub struct CompiledEmoteDisplayRules {
    by_provider: [FxHashMap<Box<str>, CompiledEmoteRule>; PROVIDER_SLOTS],
}

impl CompiledEmoteDisplayRules {
    pub fn from_rules(rules: &[EmoteDisplayRule]) -> Self {
        let mut compiled = Self::default();
        for rule in rules {
            let flags = rule.flags();
            if flags.is_none() || rule.emote.id.is_empty() {
                continue;
            }
            let Some(slot) = provider_from_key(&rule.emote.provider).and_then(provider_slot) else {
                continue;
            };
            let entry = compiled.by_provider[slot]
                .entry(Box::from(rule.emote.id.as_str()))
                .or_default();
            match &rule.scope {
                EmoteRuleScope::Everywhere => entry.everywhere = entry.everywhere.union(flags),
                EmoteRuleScope::Channel(login) => {
                    let login = normalize_channel(login);
                    if login.is_empty() {
                        continue;
                    }
                    match entry
                        .channels
                        .iter_mut()
                        .find(|(existing, _)| existing.as_ref() == login)
                    {
                        Some((_, existing)) => *existing = existing.union(flags),
                        None => entry.channels.push((login.into_boxed_str(), flags)),
                    }
                }
            }
        }
        compiled
    }

    pub fn is_empty(&self) -> bool {
        self.by_provider.iter().all(FxHashMap::is_empty)
    }

    /// Effective flags for one emote in `channel`: the Everywhere rule's flags
    /// combined with that channel's rule.
    #[inline]
    pub fn flags_for(
        &self,
        provider: EmoteProvider,
        emote_id: &str,
        channel: &str,
    ) -> EmoteDisplayFlags {
        let Some(slot) = provider_slot(provider) else {
            return EmoteDisplayFlags::default();
        };
        let Some(rule) = self.by_provider[slot].get(emote_id) else {
            return EmoteDisplayFlags::default();
        };
        rule.channels
            .iter()
            .filter(|(login, _)| channel_matches(login, channel))
            .fold(rule.everywhere, |flags, (_, channel_flags)| {
                flags.union(*channel_flags)
            })
    }
}

/// The flags stored by the rule for `emote` at exactly `scope`.
pub fn rule_flags_at(
    rules: &[EmoteDisplayRule],
    emote: &EmoteRef,
    scope: &EmoteRuleScope,
) -> EmoteDisplayFlags {
    rules
        .iter()
        .find(|rule| rule.targets(emote, scope))
        .map(EmoteDisplayRule::flags)
        .unwrap_or_default()
}

/// The scopes whose rules turn `flag` on for `emote` in `channel`. Clearing
/// `flag` at each of them turns the effect off there.
pub fn scopes_applying(
    rules: &[EmoteDisplayRule],
    emote: &EmoteRef,
    channel: &str,
    flag: EmoteDisplayFlag,
) -> Vec<EmoteRuleScope> {
    rules
        .iter()
        .filter(|rule| {
            same_emote(&rule.emote, emote)
                && rule.flags().get(flag)
                && rule.scope.applies_to(channel)
        })
        .map(|rule| rule.scope.clone())
        .collect()
}

/// Apply one change. A rule left with no flags is removed. Returns the change
/// that restores the previous state, or `None` when nothing changed.
pub fn apply_change(
    rules: &mut Vec<EmoteDisplayRule>,
    change: &EmoteRuleChange,
) -> Option<EmoteRuleChange> {
    if provider_from_key(&change.emote.provider).is_none() || change.emote.id.is_empty() {
        return None;
    }
    let scope = match &change.scope {
        EmoteRuleScope::Channel(login) => {
            let login = normalize_channel(login);
            if login.is_empty() {
                return None;
            }
            EmoteRuleScope::Channel(login)
        }
        EmoteRuleScope::Everywhere => EmoteRuleScope::Everywhere,
    };
    let index = rules
        .iter()
        .position(|rule| rule.targets(&change.emote, &scope));
    let previous = index.is_some_and(|index| rules[index].flags().get(change.flag));
    if previous == change.value {
        return None;
    }

    let index = match index {
        Some(index) => index,
        None => {
            rules.push(EmoteDisplayRule {
                emote: change.emote.clone(),
                scope: scope.clone(),
                show_as_text: false,
                still_image: false,
            });
            rules.len() - 1
        }
    };
    let rule = &mut rules[index];
    if !change.emote.name.is_empty() {
        rule.emote.name.clone_from(&change.emote.name);
    }
    match change.flag {
        EmoteDisplayFlag::ShowAsText => rule.show_as_text = change.value,
        EmoteDisplayFlag::StillImage => rule.still_image = change.value,
    }
    if rule.flags().is_none() {
        rules.remove(index);
    }

    Some(EmoteRuleChange {
        emote: change.emote.clone(),
        scope,
        flag: change.flag,
        value: previous,
    })
}

/// Set one flag of an existing rule in place. Unlike [`apply_change`] this
/// never adds or removes a rule, so a settings row survives being switched off,
/// and it leaves the other flag as it currently is.
pub fn set_rule_flag(
    rules: &mut [EmoteDisplayRule],
    emote: &EmoteRef,
    scope: &EmoteRuleScope,
    flag: EmoteDisplayFlag,
    value: bool,
) -> bool {
    let Some(rule) = rules.iter_mut().find(|rule| rule.targets(emote, scope)) else {
        return false;
    };
    let slot = match flag {
        EmoteDisplayFlag::ShowAsText => &mut rule.show_as_text,
        EmoteDisplayFlag::StillImage => &mut rule.still_image,
    };
    let changed = *slot != value;
    *slot = value;
    changed
}

/// Repair a loaded or imported list: lowercase channel logins without `#`,
/// drop rules with no id, an unknown provider or an empty channel, and merge
/// duplicates of one emote and scope into the first, combining their flags.
/// Rules with no flags stay, as a switched-off settings row does.
pub fn normalize_rules(rules: &mut Vec<EmoteDisplayRule>) {
    let mut normalized: Vec<EmoteDisplayRule> = Vec::with_capacity(rules.len());
    for mut rule in rules.drain(..) {
        rule.emote.id = rule.emote.id.trim().to_string();
        if rule.emote.id.is_empty() || provider_from_key(&rule.emote.provider).is_none() {
            continue;
        }
        if let EmoteRuleScope::Channel(login) = &mut rule.scope {
            *login = normalize_channel(login);
            if login.is_empty() {
                continue;
            }
        }
        match normalized
            .iter_mut()
            .find(|existing| existing.targets(&rule.emote, &rule.scope))
        {
            Some(existing) => {
                existing.show_as_text |= rule.show_as_text;
                existing.still_image |= rule.still_image;
            }
            None => normalized.push(rule),
        }
    }
    *rules = normalized;
}

/// Deserialize a rule list, skipping entries that do not parse, so one bad
/// rule cannot make the whole settings file unloadable. A value that is not a
/// list reads as no rules.
pub fn deserialize_rules_lenient<'de, D>(deserializer: D) -> Result<Vec<EmoteDisplayRule>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Array(entries) = value else {
        return Ok(Vec::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect())
}

/// Remove the rule for `emote` at `scope`.
pub fn remove_rule(
    rules: &mut Vec<EmoteDisplayRule>,
    emote: &EmoteRef,
    scope: &EmoteRuleScope,
) -> bool {
    let before = rules.len();
    rules.retain(|rule| !rule.targets(emote, scope));
    rules.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kekw() -> EmoteRef {
        EmoteRef::new("60b0", "SevenTV", "KEKW")
    }

    fn rule(scope: EmoteRuleScope, show_as_text: bool, still_image: bool) -> EmoteDisplayRule {
        EmoteDisplayRule {
            emote: kekw(),
            scope,
            show_as_text,
            still_image,
        }
    }

    fn change(scope: EmoteRuleScope, flag: EmoteDisplayFlag, value: bool) -> EmoteRuleChange {
        EmoteRuleChange {
            emote: kekw(),
            scope,
            flag,
            value,
        }
    }

    #[test]
    fn emote_display_rules_empty_snapshot_is_a_fast_no_op() {
        let compiled = CompiledEmoteDisplayRules::from_rules(&[]);
        assert!(compiled.is_empty());
        assert!(
            compiled
                .flags_for(EmoteProvider::SevenTV, "60b0", "xqc")
                .is_none()
        );

        let inert = CompiledEmoteDisplayRules::from_rules(&[rule(
            EmoteRuleScope::Everywhere,
            false,
            false,
        )]);
        assert!(inert.is_empty());
    }

    #[test]
    fn emote_display_rules_scope_limits_channel_rules() {
        let compiled = CompiledEmoteDisplayRules::from_rules(&[rule(
            EmoteRuleScope::channel("#XQC"),
            true,
            false,
        )]);
        let in_channel = compiled.flags_for(EmoteProvider::SevenTV, "60b0", "#xqc");
        assert!(in_channel.show_as_text);
        assert!(
            compiled
                .flags_for(EmoteProvider::SevenTV, "60b0", "Xqc")
                .show_as_text
        );
        assert!(
            compiled
                .flags_for(EmoteProvider::SevenTV, "60b0", "forsen")
                .is_none()
        );
        assert!(
            compiled
                .flags_for(EmoteProvider::BTTV, "60b0", "xqc")
                .is_none(),
            "the provider is part of the emote identity"
        );
    }

    #[test]
    fn emote_display_rules_everywhere_and_channel_flags_combine() {
        let compiled = CompiledEmoteDisplayRules::from_rules(&[
            rule(EmoteRuleScope::Everywhere, false, true),
            rule(EmoteRuleScope::channel("xqc"), true, false),
        ]);
        assert_eq!(
            compiled.flags_for(EmoteProvider::SevenTV, "60b0", "xqc"),
            EmoteDisplayFlags {
                show_as_text: true,
                still_image: true,
            }
        );
        assert_eq!(
            compiled.flags_for(EmoteProvider::SevenTV, "60b0", "forsen"),
            EmoteDisplayFlags {
                show_as_text: false,
                still_image: true,
            }
        );
    }

    #[test]
    fn emote_display_rules_unknown_and_emoji_providers_never_match() {
        let mut unknown = rule(EmoteRuleScope::Everywhere, true, true);
        unknown.emote.provider = "UnknownProvider".to_string();
        let mut emoji = rule(EmoteRuleScope::Everywhere, true, true);
        emoji.emote.provider = "Emoji".to_string();
        let compiled = CompiledEmoteDisplayRules::from_rules(&[unknown.clone(), emoji]);
        assert!(compiled.is_empty());
        assert!(
            compiled
                .flags_for(EmoteProvider::Emoji, "60b0", "xqc")
                .is_none()
        );

        let mut rules = Vec::new();
        let mut unknown_change = change(
            EmoteRuleScope::Everywhere,
            EmoteDisplayFlag::ShowAsText,
            true,
        );
        unknown_change.emote = unknown.emote;
        assert_eq!(apply_change(&mut rules, &unknown_change), None);
        assert!(rules.is_empty());
    }

    #[test]
    fn emote_display_rules_flags_are_independent_and_pruned_together() {
        let mut rules = Vec::new();
        let undo_text = apply_change(
            &mut rules,
            &change(
                EmoteRuleScope::Everywhere,
                EmoteDisplayFlag::ShowAsText,
                true,
            ),
        )
        .expect("adds the rule");
        assert_eq!(
            undo_text,
            change(
                EmoteRuleScope::Everywhere,
                EmoteDisplayFlag::ShowAsText,
                false
            )
        );
        apply_change(
            &mut rules,
            &change(
                EmoteRuleScope::Everywhere,
                EmoteDisplayFlag::StillImage,
                true,
            ),
        )
        .expect("sets the second flag");
        assert_eq!(rules, vec![rule(EmoteRuleScope::Everywhere, true, true)]);

        // Turning text off keeps the still flag.
        apply_change(&mut rules, &undo_text).expect("clears text");
        assert_eq!(rules, vec![rule(EmoteRuleScope::Everywhere, false, true)]);

        // The last flag off removes the rule; the inverse restores it.
        let undo_still = apply_change(
            &mut rules,
            &change(
                EmoteRuleScope::Everywhere,
                EmoteDisplayFlag::StillImage,
                false,
            ),
        )
        .expect("clears still");
        assert!(rules.is_empty());
        apply_change(&mut rules, &undo_still).expect("undo restores");
        assert_eq!(rules, vec![rule(EmoteRuleScope::Everywhere, false, true)]);

        assert_eq!(
            apply_change(
                &mut rules,
                &change(
                    EmoteRuleScope::Everywhere,
                    EmoteDisplayFlag::StillImage,
                    true
                ),
            ),
            None,
            "an unchanged flag is not a change"
        );
    }

    #[test]
    fn emote_display_rules_report_the_scopes_that_apply() {
        let rules = vec![
            rule(EmoteRuleScope::Everywhere, true, false),
            rule(EmoteRuleScope::channel("xqc"), true, true),
            rule(EmoteRuleScope::channel("forsen"), true, false),
        ];
        assert_eq!(
            scopes_applying(&rules, &kekw(), "#xqc", EmoteDisplayFlag::ShowAsText),
            vec![EmoteRuleScope::Everywhere, EmoteRuleScope::channel("xqc")]
        );
        assert_eq!(
            scopes_applying(&rules, &kekw(), "xqc", EmoteDisplayFlag::StillImage),
            vec![EmoteRuleScope::channel("xqc")]
        );
        assert_eq!(
            rule_flags_at(&rules, &kekw(), &EmoteRuleScope::channel("forsen")),
            EmoteDisplayFlags {
                show_as_text: true,
                still_image: false,
            }
        );
    }

    #[test]
    fn emote_display_rules_settings_edits_keep_and_remove_rows() {
        let mut rules = vec![rule(EmoteRuleScope::channel("xqc"), true, true)];
        let scope = EmoteRuleScope::channel("xqc");
        assert!(set_rule_flag(
            &mut rules,
            &kekw(),
            &scope,
            EmoteDisplayFlag::ShowAsText,
            false
        ));
        assert_eq!(
            rules,
            vec![rule(EmoteRuleScope::channel("xqc"), false, true)],
            "one flag changes; the other keeps its current value"
        );
        assert!(!set_rule_flag(
            &mut rules,
            &kekw(),
            &scope,
            EmoteDisplayFlag::ShowAsText,
            false
        ));
        assert!(set_rule_flag(
            &mut rules,
            &kekw(),
            &scope,
            EmoteDisplayFlag::StillImage,
            false
        ));
        assert_eq!(rules.len(), 1, "a switched-off settings row stays listed");
        assert!(!set_rule_flag(
            &mut rules,
            &kekw(),
            &EmoteRuleScope::Everywhere,
            EmoteDisplayFlag::StillImage,
            true
        ));
        assert!(!remove_rule(
            &mut rules,
            &kekw(),
            &EmoteRuleScope::Everywhere
        ));
        assert!(remove_rule(&mut rules, &kekw(), &scope));
        assert!(rules.is_empty());
    }

    #[test]
    fn emote_display_rules_scopes_match_after_channel_normalization() {
        let mut rules = vec![rule(
            EmoteRuleScope::Channel(" #XQC ".to_string()),
            true,
            false,
        )];
        assert!(rule_flags_at(&rules, &kekw(), &EmoteRuleScope::channel("xqc")).show_as_text);
        assert_eq!(
            apply_change(
                &mut rules,
                &change(
                    EmoteRuleScope::channel("Xqc"),
                    EmoteDisplayFlag::ShowAsText,
                    true
                ),
            ),
            None,
            "the stored rule already applies"
        );
        assert!(remove_rule(
            &mut rules,
            &kekw(),
            &EmoteRuleScope::channel("xqc")
        ));
    }

    #[test]
    fn emote_display_rules_normalize_repairs_loaded_lists() {
        let mut no_id = rule(EmoteRuleScope::Everywhere, true, false);
        no_id.emote.id = "  ".to_string();
        let mut unknown = rule(EmoteRuleScope::Everywhere, true, false);
        unknown.emote.provider = "UnknownProvider".to_string();
        let mut emoji = rule(EmoteRuleScope::Everywhere, true, false);
        emoji.emote.provider = "Emoji".to_string();
        let mut rules = vec![
            rule(EmoteRuleScope::Channel("#XQC".to_string()), true, false),
            no_id,
            unknown,
            emoji,
            rule(EmoteRuleScope::Channel("#".to_string()), true, false),
            rule(EmoteRuleScope::channel("xqc"), false, true),
            rule(EmoteRuleScope::Everywhere, false, false),
        ];
        normalize_rules(&mut rules);
        assert_eq!(
            rules,
            vec![
                rule(EmoteRuleScope::channel("xqc"), true, true),
                rule(EmoteRuleScope::Everywhere, false, false),
            ]
        );
    }

    #[test]
    fn emote_display_rules_lenient_deserialize_skips_bad_entries() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default, deserialize_with = "deserialize_rules_lenient")]
            rules: Vec<EmoteDisplayRule>,
        }

        let parsed: Holder = serde_json::from_str(
            r#"{"rules":[
                {"emote":{"id":"60b0","provider":"SevenTV","name":"KEKW"},"show_as_text":true},
                {"emote":"not a rule"},
                {"emote":{"id":"60b0","provider":"SevenTV","name":"KEKW"},"scope":{"galaxy":1}},
                7
            ]}"#,
        )
        .expect("settings with a bad rule still parse");
        assert_eq!(
            parsed.rules,
            vec![rule(EmoteRuleScope::Everywhere, true, false)]
        );

        let not_a_list: Holder =
            serde_json::from_str(r#"{"rules":{"oops":true}}"#).expect("a non-list parses");
        assert!(not_a_list.rules.is_empty());
        let missing: Holder = serde_json::from_str("{}").expect("a missing list parses");
        assert!(missing.rules.is_empty());
    }

    #[test]
    fn emote_display_rules_deserialize_with_defaults() {
        let parsed: EmoteDisplayRule = serde_json::from_str(
            r#"{"emote":{"id":"60b0","provider":"SevenTV","name":"KEKW"},"still_image":true}"#,
        )
        .expect("rule parses");
        assert_eq!(parsed, rule(EmoteRuleScope::Everywhere, false, true));

        let channel: EmoteDisplayRule = serde_json::from_str(
            r#"{"emote":{"id":"60b0","provider":"SevenTV","name":"KEKW"},"scope":{"channel":"xqc"},"show_as_text":true}"#,
        )
        .expect("channel rule parses");
        assert_eq!(channel.scope, EmoteRuleScope::channel("xqc"));
        assert_eq!(
            serde_json::to_value(&channel.scope).expect("scope serializes"),
            serde_json::json!({ "channel": "xqc" })
        );
    }
}
