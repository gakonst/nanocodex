//! Source-grounded replacement regressions: original 10072e0a0 prefers
//! NSAttributedString.string after 100620590 cannot map visible text.
//! No native UI or original service is executed.
use skyre::{
    ax::Node,
    native::{
        render::{self, AttributedInput, DeferredRun},
        url::Shortener,
    },
    rich_text::{AttributedRun, TextStyle},
    selection::{MappedText, Mode, TextRange, select_node},
};

fn input(source: &str, style: TextStyle) -> AttributedInput {
    AttributedInput {
        source: source.into(),
        runs: vec![DeferredRun {
            run: AttributedRun {
                range: TextRange {
                    location: 0,
                    length: source.encode_utf16().count(),
                },
                style,
            },
            link: None,
            attachment_url: None,
        }],
    }
}
fn field(source: &str) -> Node {
    Node {
        identity: "owned-text".into(),
        role: "AXTextArea".into(),
        value: Some(source.into()),
        settable: true,
        ..Default::default()
    }
}
fn rendered(source: &str, style: TextStyle) -> Node {
    let mut node = field(source);
    input(source, style)
        .render_into(&mut node, &mut Shortener::default())
        .unwrap();
    node
}

#[test]
fn unique_generated_heading_and_link_destination_cannot_fall_back_to_native_offsets() {
    for (style, generated) in [
        (
            TextStyle {
                heading: 1,
                ..Default::default()
            },
            "# ",
        ),
        (
            TextStyle {
                link: Some("https://example.invalid/unique".into()),
                ..Default::default()
            },
            "https://example.invalid/unique",
        ),
    ] {
        let node = rendered("abc", style);
        assert_eq!(node.attributed_source.as_deref(), Some("abc"));
        assert_eq!(node.value.as_ref().unwrap().matches(generated).count(), 1);
        for mode in [Mode::Text, Mode::CursorBefore, Mode::CursorAfter] {
            assert!(select_node(&node, generated, None, None, mode).is_err());
        }
        assert_eq!(
            select_node(&node, "b", None, None, Mode::Text).unwrap(),
            TextRange {
                location: 1,
                length: 1
            }
        );
    }
}

#[test]
fn raw_attributed_fallback_retains_trimmed_whitespace_and_utf16_unicode() {
    let node = rendered(
        " \tβ🧪\n ",
        TextStyle {
            link: Some("url".into()),
            ..Default::default()
        },
    );
    assert_eq!(node.value.as_deref(), Some("[β🧪](url)"));
    // The exact raw match is absent from the rendered text, forcing fallback.
    for (mode, expected) in [
        (
            Mode::Text,
            TextRange {
                location: 3,
                length: 4,
            },
        ),
        (
            Mode::CursorBefore,
            TextRange {
                location: 3,
                length: 0,
            },
        ),
        (
            Mode::CursorAfter,
            TextRange {
                location: 7,
                length: 0,
            },
        ),
    ] {
        assert_eq!(
            select_node(&node, "🧪\n ", None, None, mode).unwrap(),
            expected
        );
    }
    assert_eq!(
        select_node(&node, "β🧪", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 2,
            length: 3
        }
    );
    // A genuine raw prefix is valid even though generated Markdown interrupts
    // that context in the display; fallback checks it against the raw source.
    assert_eq!(
        select_node(&node, "β", Some(" \t"), Some("🧪"), Mode::Text).unwrap(),
        TextRange {
            location: 2,
            length: 1
        }
    );
}

#[test]
fn an_empty_attributed_source_remains_distinct_from_absent_source() {
    let mut node = field("generated");
    let mut mapped = MappedText::default();
    mapped.syntax("generated");
    node.mapped_value = Some(mapped);
    node.attributed_source = Some(String::new());
    assert!(select_node(&node, "generated", None, None, Mode::Text).is_err());
    node.attributed_source = None;
    node.mapped_value = None;
    assert_eq!(
        select_node(&node, "generated", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 0,
            length: 9
        }
    );
}

#[test]
fn full_ui_preparation_retains_raw_source_in_main_and_focus_views() {
    let node = field(" \tβ🧪\n ");
    let raw = input(
        node.value.as_ref().unwrap(),
        TextStyle {
            link: Some("url".into()),
            ..Default::default()
        },
    );
    let prepared = render::prepare_full_ui(
        Node {
            identity: "owned-window".into(),
            role: "AXWindow".into(),
            title: Some("Owned".into()),
            children: vec![node.clone()],
            ..Default::default()
        },
        Some(Box::new(node)),
        "owned",
        &mut Shortener::default(),
        &mut |node, urls| raw.render_into(node, urls),
    )
    .unwrap();
    for node in [
        prepared.by_identity("owned-text").unwrap(),
        prepared.focus_tree.as_ref().unwrap(),
    ] {
        assert_eq!(node.attributed_source.as_deref(), Some(" \tβ🧪\n "));
        assert_eq!(
            select_node(node, "🧪\n ", None, None, Mode::Text).unwrap(),
            TextRange {
                location: 3,
                length: 4
            }
        );
        assert!(select_node(node, "url", None, None, Mode::Text).is_err());
    }
}

#[test]
fn equal_rendered_text_with_changed_raw_offsets_invalidates_normal_refetch() {
    let style = TextStyle {
        link: Some("url".into()),
        ..Default::default()
    };
    let before = rendered(" abc ", style.clone());
    let after = rendered("abc ", style);
    assert_eq!(before.value, after.value);
    assert_eq!(
        select_node(&before, "a", None, None, Mode::Text)
            .unwrap()
            .location,
        1
    );
    assert_eq!(
        select_node(&after, "a", None, None, Mode::Text)
            .unwrap()
            .location,
        0
    );
    // Stronger replacement safeguard than the recovered eleven-field check:
    // an old map may not be reused after invisible raw source edits.
    assert!(!before.semantic_eq(&after, false));
    assert!(before.semantic_eq(&after, true));
}

#[test]
fn retained_selection_source_budget_is_checked_before_revision_publication() {
    let mut node = field("abc");
    node.attributed_source = Some(" ".repeat(16 * 1024 * 1024 + 1));
    let error = render::prepare_full_ui(
        node,
        None,
        "owned",
        &mut Shortener::default(),
        &mut |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(error.message.contains("retained selection source bound"));
}
