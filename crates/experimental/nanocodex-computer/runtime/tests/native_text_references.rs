//! Recovered attributed renderer branch contracts (10073dd80/100740fd0).
//! Replacement-only fixtures; no original service or native UI is invoked.
use skyre::{
    rich_text::{AttributedRun, ListStyle, TextAttachment, TextStyle, render_attributed},
    selection::{MappedText, Mode, TextRange, select},
};
fn render(source: &str, style: TextStyle) -> MappedText {
    render_attributed(
        source,
        &[AttributedRun {
            range: TextRange {
                location: 0,
                length: source.encode_utf16().count(),
            },
            style,
        }],
    )
    .unwrap()
}
fn attachment(role: Option<&str>, description: Option<&str>) -> TextStyle {
    TextStyle {
        attachment: Some(TextAttachment {
            role_description: role.map(str::to_owned),
            description: description.map(str::to_owned),
            image_url: None,
        }),
        ..Default::default()
    }
}
fn range(mapped: &MappedText, text: &str) -> skyre::Result<TextRange> {
    mapped.source_range(select(&mapped.text, text, None, None, Mode::Text)?)
}
#[test]
fn recovered_attachment_fallback_distinguishes_absence_empty_and_source_caption() {
    for (source, role, description, expected) in [
        ("\u{fffc}", None, None, "[attachment]"),
        ("\u{fffc}", Some("PDF"), Some("Report"), "[PDF: Report]"),
        ("\u{fffc}", Some("PDF"), Some(""), "[PDF]"),
        ("\u{fffc}", Some(""), None, "[]"),
        (
            "Caption",
            Some("PDF"),
            Some("Description"),
            "[PDF: Caption]",
        ),
        ("  Caption  ", None, None, "[attachment:   Caption  ]"),
    ] {
        let mapped = render(source, attachment(role, description));
        assert_eq!(mapped.text, expected);
        assert_eq!(mapped.source_offsets.len(), expected.encode_utf16().count());
        assert!(mapped.source_offsets.iter().all(Option::is_none));
        assert!(
            range(&mapped, expected)
                .unwrap_err()
                .message
                .contains("no source text")
        );
    }
}
#[test]
fn recovered_multiline_attachment_retains_only_original_suffix_offsets() {
    for newline in [
        "\n", "\r", "\r\n", "\u{b}", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}",
    ] {
        let source = format!("α{newline}🧪");
        let mapped = render(&source, attachment(Some("PDF"), Some("Report")));
        assert_eq!(mapped.text, format!("[PDF: Report] {source}"));
        assert!(mapped.source_offsets[..14].iter().all(Option::is_none));
        assert_eq!(
            mapped.source_offsets[14..],
            (0..source.encode_utf16().count())
                .map(Some)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            range(&mapped, "🧪").unwrap(),
            TextRange {
                location: 1 + newline.encode_utf16().count(),
                length: 2
            }
        );
        assert!(range(&mapped, "PDF: Report").is_err());
    }
}
#[test]
fn recovered_link_trims_utf16_label_before_font_styles_and_keeps_destination_generated() {
    let mapped = render(
        " \tβ🧪\n ",
        TextStyle {
            link: Some("//example.invalid/(x)".into()),
            bold: true,
            underline: true,
            ..Default::default()
        },
    );
    assert_eq!(mapped.text, "**<u>[β🧪](//example.invalid/(x))</u>**");
    assert_eq!(
        mapped
            .source_offsets
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        [2, 3, 4]
    );
    assert_eq!(
        range(&mapped, "β🧪").unwrap(),
        TextRange {
            location: 2,
            length: 3
        }
    );
    assert!(range(&mapped, "//example.invalid/(x)").is_err());
    assert_eq!(
        render(
            "   ",
            TextStyle {
                link: Some("u".into()),
                ..Default::default()
            }
        )
        .text,
        "[](u)"
    );
}
#[test]
fn recovered_attachment_and_link_fragments_are_not_reescaped_by_paragraph_styles() {
    let mut style = attachment(Some("PDF"), Some("Report"));
    style.bold = true;
    style.list = Some(ListStyle::from_accessibility(1, 2, "2."));
    assert_eq!(render("\u{fffc}", style).text, "    2. **[PDF: Report]**");
    let mapped = render(
        " caption ",
        TextStyle {
            attachment: Some(TextAttachment {
                image_url: Some("//example.invalid/i".into()),
                ..Default::default()
            }),
            italic: true,
            ..Default::default()
        },
    );
    assert_eq!(mapped.text, "*![caption](//example.invalid/i)*");
    assert_eq!(
        range(&mapped, "caption").unwrap(),
        TextRange {
            location: 1,
            length: 7
        }
    );
    // Pure image branch is retained even though current native shortening
    // suppresses image URLs and therefore chooses the fallback path.
    let mapped = render(
        "\u{fffc}",
        TextStyle {
            attachment: Some(TextAttachment {
                description: Some(" image ".into()),
                image_url: Some("u".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    assert_eq!(mapped.text, "![image](u)");
    assert!(mapped.source_offsets.iter().all(Option::is_none));
}
#[test]
fn reference_rendering_rejects_disjoint_selection_and_excessive_metadata() {
    let source = " a  b ";
    let mapped = render_attributed(
        source,
        &[
            AttributedRun {
                range: TextRange {
                    location: 0,
                    length: 3,
                },
                style: TextStyle {
                    link: Some("u".into()),
                    ..Default::default()
                },
            },
            AttributedRun {
                range: TextRange {
                    location: 3,
                    length: 3,
                },
                style: TextStyle {
                    link: Some("v".into()),
                    ..Default::default()
                },
            },
        ],
    )
    .unwrap();
    assert_eq!(mapped.text, "[a](u)[b](v)");
    assert!(
        range(&mapped, "a](u)[b")
            .unwrap_err()
            .message
            .contains("disjoint source ranges")
    );
    assert!(
        render_attributed(
            "a",
            &[AttributedRun {
                range: TextRange {
                    location: 0,
                    length: 1
                },
                style: TextStyle {
                    link: Some("x".repeat(65537)),
                    ..Default::default()
                }
            }]
        )
        .unwrap_err()
        .message
        .contains("metadata exceeds")
    );
}
#[test]
fn recovered_cancel_filter_is_exactly_menu_bar_and_menu_item_in_full_ui() {
    use skyre::ax::{
        Node,
        transform::{Context, apply},
    };
    for role in ["AXMenuBar", "AXMenuItem", "AXMenuBarItem", "AXButton"] {
        let node = Node {
            role: role.into(),
            title: Some("Fixture".into()),
            actions: vec!["AXCancel".into()],
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            apply(node.clone(), "fixture", Context::FullUi)
                .actions
                .contains(&"AXCancel".into()),
            !matches!(role, "AXMenuBar" | "AXMenuItem")
        );
        for context in [Context::EventStream, Context::Informational] {
            assert_eq!(
                apply(node.clone(), "fixture", context).actions,
                ["AXCancel"]
            );
        }
    }
}
#[test]
fn recovered_generic_link_uses_only_url_display_and_keeps_generated_offsets() {
    use skyre::ax::{
        Node,
        transform::{Context, apply},
    };
    for title in [None, Some("A provider label".to_owned())] {
        let node = Node {
            identity: "link".into(),
            role: "AXLink".into(),
            title,
            url: Some("https://www.example.invalid/a?q=1#f".into()),
            focused: true,
            focusable: true,
            enabled: true,
            actions: vec!["AXPress".into()],
            children: vec![Node {
                role: "AXStaticText".into(),
                value: Some("label child".into()),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mapped = apply(node, "fixture", Context::FullUi);
        assert_eq!(mapped.role, "AXStaticText");
        assert_eq!(mapped.role_description.as_deref(), Some("link"));
        assert_eq!(mapped.value.as_deref(), Some("example.invalid/a?q=1#f"));
        assert!(mapped.children.is_empty());
        assert!(mapped.url.is_none());
        let value = mapped.mapped_value.unwrap();
        assert_eq!(value.text, mapped.value.unwrap());
        assert!(value.source_offsets.iter().all(Option::is_none));
    }
    for url in [
        None,
        Some("data:text/plain,owned".into()),
        Some("webdoc:owned".into()),
    ] {
        let node = Node {
            role: "AXLink".into(),
            title: Some("label".into()),
            url: url.clone(),
            enabled: true,
            ..Default::default()
        };
        let result = apply(node, "fixture", Context::FullUi);
        assert_eq!(result.role, "AXLink");
        assert_eq!(result.url, url);
        assert!(result.mapped_value.is_none());
    }
}
#[cfg(target_os = "macos")]
#[test]
fn mapped_link_trim_matches_independent_foundation_swift_oracle() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../fixtures/native/text_reference_trim.json")).unwrap();
    assert_eq!(rows.len(), 10);
    for row in rows {
        let source = row["source"].as_str().unwrap();
        let trimmed = row["trimmed"].as_str().unwrap();
        let mapped = render(
            source,
            TextStyle {
                link: Some("u".into()),
                ..Default::default()
            },
        );
        assert_eq!(mapped.text, format!("[{trimmed}](u)"));
        let mut expected = vec![None];
        if !trimmed.is_empty() {
            if let Some(start) = row["location"].as_u64() {
                let length = row["length"].as_u64().unwrap();
                expected.extend((start as usize..(start + length) as usize).map(Some));
            } else {
                expected.extend(trimmed.encode_utf16().map(|_| None));
            }
        }
        expected.extend([None; 4]);
        assert_eq!(mapped.source_offsets, expected, "{source:?}");
    }
}
#[cfg(target_os = "macos")]
#[test]
fn native_selection_preserves_leading_bom_in_source_needle_and_context_offsets() {
    assert_eq!(
        select("\u{feff}🧪value", "value", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 3,
            length: 5
        }
    );
    assert_eq!(
        select("\u{feff}🧪value", "\u{feff}🧪", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 0,
            length: 3
        }
    );
    assert_eq!(
        select(
            "\u{feff}value value",
            "value",
            Some("\u{feff}"),
            None,
            Mode::Text
        )
        .unwrap(),
        TextRange {
            location: 1,
            length: 5
        }
    );
    assert!(select("value", "\u{feff}value", None, None, Mode::Text).is_err());
}
#[test]
fn attributed_output_bound_includes_final_style_closing_syntax() {
    let source = "x".repeat(4 * 1024 * 1024 - 2);
    let result = render_attributed(
        &source,
        &[AttributedRun {
            range: TextRange {
                location: 0,
                length: source.len(),
            },
            style: TextStyle {
                bold: true,
                ..Default::default()
            },
        }],
    );
    assert!(
        result
            .unwrap_err()
            .message
            .contains("output exceeds rendering bounds")
    );
}
