//! The fonts installed on the machine, for the "Installed on this PC" section
//! of the font picker.
//!
//! Read from the registry rather than a font directory: the value NAMES are the
//! face names Windows resolves `font-family` against, while the file names next
//! to them are not (`segoeuib.ttf` is "Segoe UI Bold"). Both hives are read;
//! HKCU holds per-user installs, which are invisible in HKLM.

/// Registry entries are `"Arial (TrueType)"`; a few pack several faces into one
/// value separated by `&`.
#[cfg(windows)]
fn clean(value: &str) -> Vec<String> {
    let base = match value.rfind(" (") {
        Some(i) if value.ends_with(')') => &value[..i],
        _ => value,
    };
    base.split('&')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pure weight/slant words. A face whose name is another installed family plus
/// only these is a style variant, not a family, so it is folded away: listing
/// "Segoe UI", "Segoe UI Bold", "Segoe UI Light", "Segoe UI Semibold" as four
/// choices is noise when the boldness slider already covers them. Width and
/// optical words (Black, Condensed, Narrow, Display, Text) are NOT in this list
/// because they name genuinely separate families.
#[cfg(windows)]
const STYLE_WORDS: &[&str] = &[
    "bold",
    "italic",
    "oblique",
    "regular",
    "light",
    "semibold",
    "semilight",
    "demibold",
    "medium",
    "thin",
    "extralight",
    "extrabold",
    "ultralight",
    "ultrabold",
    "heavy",
    "book",
    "roman",
    "semi",
    "extra",
    "ultra",
];

/// Icon and symbol faces. Real fonts, but every preview in them is dingbats and
/// every label set in them is unreadable, so they are not offered.
#[cfg(windows)]
const SYMBOL_FONTS: &[&str] = &[
    "marlett",
    "symbol",
    "webdings",
    "wingdings",
    "wingdings 2",
    "wingdings 3",
    "mt extra",
    "bookshelf symbol 7",
    "segoe mdl2 assets",
    "segoe fluent icons",
    "hololens mdl2 assets",
    "holomdl2",
    "segoe ui symbol",
    "opensymbol",
];

#[cfg(windows)]
pub fn installed() -> Vec<String> {
    use std::collections::BTreeMap;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    const PATH: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    // Keyed lowercase so the two hives cannot yield the same family twice.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let Ok(key) = RegKey::predef(hive).open_subkey(PATH) else {
            continue;
        };
        for (value, _) in key.enum_values().flatten() {
            for name in clean(&value) {
                let k = name.to_lowercase();
                if SYMBOL_FONTS.contains(&k.as_str()) {
                    continue;
                }
                names.insert(k, name);
            }
        }
    }

    // Fold style variants into their family. Walk longest-first so "Segoe UI
    // Semibold Italic" folds even though only "Segoe UI" is a real family.
    let keys: Vec<String> = names.keys().cloned().collect();
    let mut out: Vec<String> = Vec::new();
    for k in &keys {
        let mut variant = false;
        for cut in k.match_indices(' ').map(|(i, _)| i) {
            let (head, tail) = k.split_at(cut);
            if !names.contains_key(head) {
                continue;
            }
            if tail.split_whitespace().all(|w| STYLE_WORDS.contains(&w)) {
                variant = true;
                break;
            }
        }
        if !variant {
            out.push(names[k].clone());
        }
    }
    out.sort_by_key(|s| s.to_lowercase());
    out
}

#[cfg(target_os = "linux")]
pub fn installed() -> Vec<String> {
    use std::collections::BTreeMap;
    use std::process::Command;

    // fontconfig is the desktop-standard source of family names. Requesting the
    // family field rather than walking directories also includes per-user fonts.
    let Ok(out) = Command::new("fc-list").args([":", "family"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut names = BTreeMap::new();
    for family in String::from_utf8_lossy(&out.stdout)
        .lines()
        .flat_map(|line| line.split(','))
    {
        let family = family.trim();
        if !family.is_empty() {
            names
                .entry(family.to_lowercase())
                .or_insert_with(|| family.to_string());
        }
    }
    names.into_values().collect()
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub fn installed() -> Vec<String> {
    Vec::new()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn strips_the_format_suffix_and_splits_multi_face_values() {
        assert_eq!(clean("Arial (TrueType)"), vec!["Arial"]);
        assert_eq!(
            clean("Courier 10,12,15 (VGA res)"),
            vec!["Courier 10,12,15"]
        );
        assert_eq!(
            clean("Cambria & Cambria Math (TrueType)"),
            vec!["Cambria", "Cambria Math"]
        );
        // A name that merely contains a paren stays whole.
        assert_eq!(clean("Foo (Bar) Sans"), vec!["Foo (Bar) Sans"]);
    }
}
