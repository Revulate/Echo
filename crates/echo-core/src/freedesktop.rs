//! Readers for freedesktop and KDE configuration text.

/// The value of `key` in `[section]` of an INI-style file such as KDE's
/// `kdeglobals`. KDE marks some keys with flags (`font[$e]=`); those match as
/// the plain key. The last assignment in the section wins, as in KConfig.
pub fn ini_value<'a>(contents: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut in_section = false;
    let mut value = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_section = name == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, raw_value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let name = name
            .find("[$")
            .map_or(name, |flags| name[..flags].trim_end());
        if name == key {
            value = Some(raw_value.trim());
        }
    }
    value
}

/// The family of a serialized Qt font (`"Noto Sans,10,-1,5,400,0,0,0,0,0"`).
pub fn qt_font_family(value: &str) -> Option<&str> {
    let family = value.split(',').next()?.trim();
    (!family.is_empty()).then_some(family)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KDEGLOBALS: &str = "\
[KDE]
font=Not This,10

# comment=ignored
[General]
ColorScheme=BreezeDark
font = Inter Display ,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1
fixed[$e]=JetBrains Mono,10,-1,5,400,0,0,0,0,0
fixed=Hack,10,-1,5,400,0,0,0,0,0

[WM]
font=Also Not This,10
";

    #[test]
    fn ini_value_reads_only_the_requested_section() {
        assert_eq!(
            ini_value(KDEGLOBALS, "General", "ColorScheme"),
            Some("BreezeDark")
        );
        assert_eq!(
            ini_value(KDEGLOBALS, "General", "font"),
            Some("Inter Display ,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1")
        );
        assert_eq!(
            ini_value(KDEGLOBALS, "WM", "font"),
            Some("Also Not This,10")
        );
        assert_eq!(ini_value(KDEGLOBALS, "General", "comment"), None);
        assert_eq!(ini_value(KDEGLOBALS, "Missing", "font"), None);
    }

    #[test]
    fn ini_value_treats_flagged_keys_as_plain_and_keeps_the_last() {
        assert_eq!(
            ini_value(KDEGLOBALS, "General", "fixed"),
            Some("Hack,10,-1,5,400,0,0,0,0,0")
        );
        assert_eq!(
            ini_value("[General]\nfixed[$e]=Hack,10\n", "General", "fixed"),
            Some("Hack,10")
        );
    }

    #[test]
    fn qt_font_family_takes_the_trimmed_family() {
        assert_eq!(
            qt_font_family("Inter Display ,10,-1,5,400,0,0,0,0,0"),
            Some("Inter Display")
        );
        assert_eq!(qt_font_family("Noto Sans"), Some("Noto Sans"));
        assert_eq!(qt_font_family(" ,10"), None);
        assert_eq!(qt_font_family(""), None);
    }
}
