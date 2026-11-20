//! Filter store for message filtering with expression rules.
//!
//! The evaluator supports boolean operators, typed literals, lists,
//! comparisons, string/list operators, regex literals, regex capture groups,
//! and left-to-right math.

use super::lock_utils::RwLockExt;
pub use echo_core::filter_expression::{FilterContext as TestContext, FilterExpression};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::{Arc, RwLock};

/// Filter action to take when a rule matches
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FilterAction {
    Allow,
    Hide,
    #[default]
    Highlight,
    /// Replace-matched-text mode. The boolean evaluator does not act on this
    /// variant; ignore-phrase replacements run in a dedicated pass at message
    /// build time (see `backend::filters_replace`). It exists here only so the
    /// action set stays total across the settings <-> evaluator boundary.
    Replace,
}

impl From<crate::backend::settings::FilterAction> for FilterAction {
    fn from(action: crate::backend::settings::FilterAction) -> Self {
        match action {
            crate::backend::settings::FilterAction::Allow => Self::Allow,
            crate::backend::settings::FilterAction::Hide => Self::Hide,
            crate::backend::settings::FilterAction::Highlight => Self::Highlight,
            crate::backend::settings::FilterAction::Replace => Self::Replace,
        }
    }
}

/// A single filter rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterRule {
    pub id: String,
    pub expression: String,
    pub action: FilterAction,
    pub enabled: bool,
}

impl From<crate::backend::settings::FilterRule> for FilterRule {
    fn from(rule: crate::backend::settings::FilterRule) -> Self {
        Self {
            id: rule.id,
            expression: rule.expression,
            action: rule.action.into(),
            enabled: rule.enabled,
        }
    }
}

/// Result of testing a filter against a message
#[derive(Debug, Clone)]
pub struct FilterTestResult {
    pub matched: bool,
    pub action: FilterAction,
    pub matched_text: Option<String>,
}

/// A rule `test_message` evaluates: an enabled, non-empty rule other than
/// Replace, with its compiled expression (`None` when it does not parse; such
/// a rule still yields an unmatched result).
type CompiledRule = (FilterAction, Option<Arc<FilterExpression>>);

#[derive(Debug, Default)]
struct FilterState {
    filters: Vec<FilterRule>,
    compiled: HashMap<String, Result<FilterExpression, String>>,
    /// The rules `test_message` evaluates and the
    /// `settings::filters_version()` they were built from.
    rules: Option<(u64, Arc<[CompiledRule]>)>,
}

/// Filter store manager.
///
/// Settings owns persisted filter CRUD. This store owns parser/evaluator cache
/// and falls back to its local vector only for tests or direct future callers.
pub struct FilterStore {
    state: RwLock<FilterState>,
}

impl FilterStore {
    fn new() -> Self {
        Self {
            state: RwLock::new(FilterState::default()),
        }
    }

    /// Get all filters from persisted settings.
    pub fn filters(&self) -> Vec<FilterRule> {
        let persisted: Vec<FilterRule> = crate::backend::settings::get_filters()
            .into_iter()
            .map(Into::into)
            .collect();

        if persisted.is_empty() {
            self.state.read_or_recover().filters.clone()
        } else {
            persisted
        }
    }

    /// Test a message against all enabled filters.
    pub fn test_message(&self, ctx: &TestContext) -> Vec<FilterTestResult> {
        self.compiled_rules()
            .iter()
            .map(|(action, expression)| {
                let evaluation = expression
                    .as_ref()
                    .and_then(|expression| expression.evaluate(ctx).ok());
                FilterTestResult {
                    matched: evaluation.as_ref().is_some_and(|e| e.matched),
                    action: *action,
                    matched_text: evaluation.and_then(|e| e.matched_text),
                }
            })
            .collect()
    }

    /// The rules to evaluate, compiled once per filter-set version instead
    /// of cloning every rule and expression for each message. Shared
    /// expressions also keep their regexes' match caches warm.
    fn compiled_rules(&self) -> Arc<[CompiledRule]> {
        let version = crate::backend::settings::filters_version();
        if let Some((built, rules)) = &self.state.read_or_recover().rules
            && *built == version
        {
            return Arc::clone(rules);
        }
        let rules: Arc<[CompiledRule]> = self
            .filters()
            .into_iter()
            .filter(|filter| {
                filter.enabled
                    && !filter.expression.trim().is_empty()
                    && filter.action != FilterAction::Replace
            })
            .map(|filter| {
                let compiled = self.compiled_expression(&filter.expression).ok();
                (filter.action, compiled.map(Arc::new))
            })
            .collect();
        self.state.write_or_recover().rules = Some((version, Arc::clone(&rules)));
        rules
    }

    fn compiled_expression(&self, expr: &str) -> Result<FilterExpression, String> {
        let key = expr.trim().to_string();

        if let Some(cached) = self.state.read_or_recover().compiled.get(&key).cloned() {
            return cached;
        }

        let parsed = FilterExpression::parse(&key);
        self.state
            .write_or_recover()
            .compiled
            .insert(key, parsed.clone());
        parsed
    }

    pub fn validate_expression(&self, expr: &str) -> Result<(), String> {
        self.compiled_expression(expr).map(|_| ())
    }
}

pub fn validate_filter_expression(expr: &str) -> Result<(), String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err("Filter expression is required".to_string());
    }
    FILTER_STORE.validate_expression(trimmed)
}

/// Global filter store instance
pub static FILTER_STORE: LazyLock<FilterStore> = LazyLock::new(FilterStore::new);
