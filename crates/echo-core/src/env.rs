//! Reading `ECHO_*` environment switches.
//!
//! Every boolean switch is parsed here so that all of them agree on what
//! counts as "on": `1`, `true`, `yes` or `on`, ignoring case and surrounding
//! whitespace. Anything else, including `0`, an empty value and an unset
//! variable, is off. A default-on switch reads through [`flag_or`], and a switch
//! with a distinct "leave the default" state through [`tristate`].

/// Whether the boolean environment switch `name` is on.
pub fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| is_truthy(&value))
}

/// The boolean switch `name`, or `default` when it is unset or blank.
///
/// For default-on switches: any other set value is parsed by [`is_truthy`], so
/// only `1`/`true`/`yes`/`on` keep such a switch on once it is set.
pub fn flag_or(name: &str, default: bool) -> bool {
    flag_or_value(std::env::var(name).ok().as_deref(), default)
}

fn flag_or_value(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim) {
        None | Some("") => default,
        Some(value) => is_truthy(value),
    }
}

/// A switch that is forced on, forced off, or left at the caller's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tristate {
    On,
    Off,
    Default,
}

/// Read `name` as a [`Tristate`]: the [`is_truthy`] spellings are `On`,
/// `0`/`false`/`no`/`off` (any case) are `Off`, and anything else, including
/// an unset or blank variable, is `Default`.
pub fn tristate(name: &str) -> Tristate {
    tristate_value(std::env::var(name).ok().as_deref())
}

fn tristate_value(value: Option<&str>) -> Tristate {
    let Some(value) = value.map(str::trim) else {
        return Tristate::Default;
    };
    if is_truthy(value) {
        Tristate::On
    } else if ["0", "false", "no", "off"]
        .iter()
        .any(|off| value.eq_ignore_ascii_case(off))
    {
        Tristate::Off
    } else {
        Tristate::Default
    }
}

/// Whether `value` spells an enabled switch.
pub fn is_truthy(value: &str) -> bool {
    let value = value.trim();
    ["1", "true", "yes", "on"]
        .iter()
        .any(|on| value.eq_ignore_ascii_case(on))
}

/// The trimmed value of `name`, or `None` when it is unset or blank.
pub fn value(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The raw value of `name`, untrimmed and possibly not UTF-8, for variables
/// that carry a path or an opaque marker. A set but empty variable is
/// `Some`; callers decide what that means.
pub fn os_value(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

/// The value of `name` parsed as `T`, or `None` when it is unset or malformed.
pub fn parsed<T: std::str::FromStr>(name: &str) -> Option<T> {
    value(name)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{Tristate, flag_or_value, is_truthy, tristate_value};

    #[test]
    fn recognises_the_enabled_spellings() {
        for on in ["1", "true", "TRUE", "Yes", "on", " 1 "] {
            assert!(is_truthy(on), "{on:?} should be on");
        }
    }

    #[test]
    fn treats_everything_else_as_off() {
        for off in ["", "0", "false", "off", "no", "2", "enabled"] {
            assert!(!is_truthy(off), "{off:?} should be off");
        }
    }

    #[test]
    fn flag_or_uses_the_default_only_when_unset_or_blank() {
        for default in [false, true] {
            assert_eq!(flag_or_value(None, default), default);
            assert_eq!(flag_or_value(Some(""), default), default);
            assert_eq!(flag_or_value(Some("  "), default), default);
            assert!(flag_or_value(Some("1"), default));
            assert!(flag_or_value(Some(" On "), default));
            assert!(!flag_or_value(Some("0"), default));
            assert!(!flag_or_value(Some("false"), default));
            assert!(!flag_or_value(Some("enabled"), default));
        }
    }

    #[test]
    fn tristate_separates_forced_states_from_the_default() {
        for on in ["1", "true", "YES", " on "] {
            assert_eq!(tristate_value(Some(on)), Tristate::On, "{on:?}");
        }
        for off in ["0", "false", "No", " OFF "] {
            assert_eq!(tristate_value(Some(off)), Tristate::Off, "{off:?}");
        }
        for default in [None, Some(""), Some("2"), Some("auto")] {
            assert_eq!(tristate_value(default), Tristate::Default, "{default:?}");
        }
    }
}
