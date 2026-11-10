//! Shared fixture output for Echo's contract tools.
//!
//! A contract tool passes or fails on its exit status
//! (`scripts/test-linux-tools.sh`); the `fixture,<name>,pass` rows printed here
//! are only a readable log of which fixtures ran.

/// Header row for [`assert_fixture`] output.
pub const FIXTURE_HEADER: &str = "kind,name,result";

/// Assert that `actual` equals `expected` and log the fixture as passed.
#[track_caller]
pub fn assert_fixture(name: &str, actual: impl AsRef<str>, expected: &str) {
    assert_eq!(actual.as_ref(), expected, "fixture mismatch for {name}");
    println!("fixture,{name},pass");
}

/// `"true"` or `"false"`, for building fixture strings from flags.
pub fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[doc(hidden)]
pub use tracing as __tracing;

/// Stand-in for Echo's `log_<category>!` macros in tools that `#[path]`-include
/// real source: `pub(crate) use check_support::category_log as log_ui;`.
///
/// It forwards to `tracing` like the real macros, so arguments (including
/// `field = value` syntax) are type-checked and used exactly as in production.
/// No tool installs a subscriber, so nothing is printed.
#[macro_export]
macro_rules! category_log {
    ($level:ident, $($arg:tt)*) => {{
        $crate::__tracing::$level!($($arg)*)
    }};
}
