//! Independent selection policy recovered from the pinned service generator.
//! Text is immutable original reference data. This self-contained distribution
//! does not emulate Foundation resource-bundle search, localization or I/O.

const BROWSER: &str = include_str!("app_instructions/Browser.md");
const DOCUMENTS: &[(&str, &[u8])] = &[
    (
        "AppleMusic",
        include_bytes!("app_instructions/AppleMusic.md"),
    ),
    ("Clock", include_bytes!("app_instructions/Clock.md")),
    ("Notion", include_bytes!("app_instructions/Notion.md")),
    ("Numbers", include_bytes!("app_instructions/Numbers.md")),
    ("Slack", include_bytes!("app_instructions/Slack.md")),
    ("Spotify", include_bytes!("app_instructions/Spotify.md")),
    (
        "iPhone Mirroring",
        include_bytes!("app_instructions/iPhone Mirroring.md"),
    ),
];

pub(super) fn for_app(
    bundle_id: &str,
    bundle_name: Option<&str>,
    supports_http: bool,
) -> Option<String> {
    select(bundle_id, bundle_name, supports_http, |candidate| {
        DOCUMENTS
            .iter()
            .find(|(name, _)| *name == candidate)
            .map(|(_, bytes)| *bytes)
    })
}

// A missing/failed lookup or invalid UTF-8 advances to the next candidate. The
// native distribution supplies embedded bytes; tests independently cover the
// original fallback policy without adding a runtime loader or override option.
fn select<'a>(
    bundle_id: &str,
    bundle_name: Option<&str>,
    supports_http: bool,
    mut read: impl FnMut(&str) -> Option<&'a [u8]>,
) -> Option<String> {
    let mut pieces = Vec::with_capacity(2);
    if supports_http {
        pieces.push(BROWSER);
    }
    let candidates = (bundle_id == "com.apple.Music")
        .then_some("AppleMusic")
        .into_iter()
        .chain(bundle_name)
        .chain(std::iter::once(bundle_id));
    for candidate in candidates {
        if let Some(text) = read(candidate)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|text| !text.is_empty())
        {
            pieces.push(text);
            break;
        }
    }
    (!pieces.is_empty()).then(|| pieces.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn music_alias_then_optional_name_then_controller_identifier() {
        let mut calls = Vec::new();
        let result = select("com.apple.Music", Some("Renamed"), false, |name| {
            calls.push(name.to_owned());
            (name == "com.apple.Music").then_some(b"identifier fallback".as_slice())
        });
        assert_eq!(result.as_deref(), Some("identifier fallback"));
        assert_eq!(calls, ["AppleMusic", "Renamed", "com.apple.Music"]);
        calls.clear();
        let result = select("com.apple.Music", Some("Renamed"), false, |name| {
            calls.push(name.to_owned());
            Some(b"first alias")
        });
        assert_eq!(result.as_deref(), Some("first alias"));
        assert_eq!(calls, ["AppleMusic"]);
    }

    #[test]
    fn missing_empty_and_duplicate_names_are_not_rewritten() {
        for (name, expected) in [
            (None, vec!["owned.id"]),
            (Some(""), vec!["", "owned.id"]),
            (Some("owned.id"), vec!["owned.id", "owned.id"]),
        ] {
            let mut calls = Vec::new();
            assert_eq!(
                select("owned.id", name, false, |key| {
                    calls.push(key.to_owned());
                    None
                }),
                None
            );
            assert_eq!(calls, expected);
        }
        assert!(for_app("owned.id", Some("slack"), false).is_none());
        assert!(for_app("com.apple.music", None, false).is_none());
    }

    #[test]
    fn failed_empty_and_invalid_utf8_reads_continue_without_trimming() {
        let mut calls = Vec::new();
        let value = select("com.apple.Music", Some("Owned"), false, |key| {
            calls.push(key.to_owned());
            match key {
                "AppleMusic" => None,
                "Owned" => Some(b""),
                _ => Some(b" exact body\n"),
            }
        });
        assert_eq!(value.as_deref(), Some(" exact body\n"));
        assert_eq!(calls, ["AppleMusic", "Owned", "com.apple.Music"]);
        calls.clear();
        let value = select("com.apple.Music", Some("Owned"), false, |key| {
            calls.push(key.to_owned());
            if key == "AppleMusic" {
                Some(b"\xff")
            } else {
                Some(b" \n")
            }
        });
        assert_eq!(value.as_deref(), Some(" \n"));
        assert_eq!(calls, ["AppleMusic", "Owned"]);
    }

    #[test]
    fn browser_note_is_first_and_survives_an_absent_app_document() {
        assert_eq!(select("none", None, false, |_| None), None);
        assert_eq!(
            select("none", None, true, |_| None).as_deref(),
            Some(BROWSER)
        );
        assert_eq!(
            select("owned", None, true, |_| Some(b"body\n")).unwrap(),
            format!("{BROWSER}\n\nbody\n"),
        );
        assert_eq!(
            select("owned", None, false, |_| Some(b"body\n")).as_deref(),
            Some("body\n")
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(BROWSER.as_bytes())),
            "9aa74eff22fac8bfa3f1247dcf36ba484da1bd2945f08a63564a356bc543728d"
        );
        assert_eq!(BROWSER.len(), 313);
        assert!(!BROWSER.ends_with('\n'));
    }

    #[test]
    fn all_seven_resources_match_the_pinned_original_data() {
        let expected = [
            (
                "AppleMusic",
                "9bddb86ceac45973489c0c704ed163b2ef55bb2b4cb044da4810ea71f4da1a19",
            ),
            (
                "Clock",
                "0b986d7fb6ef16e4cac24ce5de4adc37a0dd82981030b7ab648402c9a2e72c23",
            ),
            (
                "Notion",
                "80669d19bb799ea90649c46dfcd77d404fa1960297519640b50ac57ad81969f7",
            ),
            (
                "Numbers",
                "a4e07c2c50d233b22f6cabcac0a301474d5317d8c763e9287cdd5763ef34c1bc",
            ),
            (
                "Slack",
                "8eb94654ac7d6bf90a75b769a36e8591c8a4f0c5af774045f9499db9b5ed434a",
            ),
            (
                "Spotify",
                "bcdfc5d2087e88baa55eef3fd8345a988c15bffe74701631bb5239780fd7b99f",
            ),
            (
                "iPhone Mirroring",
                "a4f39b49aa6400d1af1657d53de1d7cf6b0f093563704136730d84f07e458ab0",
            ),
        ];
        assert_eq!(DOCUMENTS.len(), expected.len());
        for ((name, bytes), (original_name, hash)) in DOCUMENTS.iter().zip(expected) {
            assert_eq!(*name, original_name);
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), hash);
            assert_eq!(
                for_app("owned.id", Some(name), false).as_deref(),
                Some(std::str::from_utf8(bytes).unwrap())
            );
        }
        // Native generation must not duplicate the later JS Numbers suppression.
        assert!(for_app("com.apple.iWork.Numbers", Some("Numbers"), false).is_some());
    }
}
