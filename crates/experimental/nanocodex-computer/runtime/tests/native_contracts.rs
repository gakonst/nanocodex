//! Independent assertions derived from retained native contracts. These execute
//! replacement code only; they are not labelled original-service/live tests.
use skyre::{
    ax::{
        DiffOptions, Node, Revision,
        transform::{self, Context},
    },
    selection::{MappedText, Mode, TextRange, select_node},
};
fn leaf(id: &str, role: &str, value: Option<&str>) -> Node {
    Node {
        identity: id.into(),
        role: role.into(),
        value: value.map(str::to_owned),
        enabled: true,
        ..Default::default()
    }
}
#[test]
fn original_attributed_style_delimiters_paragraphs_lists_and_transitions_preserve_ranges() {
    use skyre::rich_text::{AttributedRun, ListStyle, TextStyle, render_attributed};
    let rendered = |source: &str, style: TextStyle| {
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
    };
    for (name, expected) in [
        ("Title", "# α🧪"),
        ("Heading", "## α🧪"),
        ("Subheading", "### α🧪"),
        ("Fixed width", "\u{60}\u{60}\u{60}\nα🧪\n\u{60}\u{60}\u{60}"),
        (
            "Collapsed",
            "<details><summary>\nα🧪\n</summary>(collapsed content is hidden)</details>",
        ),
        ("Body", "α🧪"),
        ("Contains paragraphs", "α🧪"),
        ("Expanded", "α🧪"),
        ("heading", "α🧪"),
    ] {
        let mut style = TextStyle::default();
        style.accessibility_style_names(name);
        let mapped = rendered("α🧪", style);
        assert_eq!(mapped.text, expected, "{name}");
        assert_eq!(mapped.source_offsets.len(), expected.encode_utf16().count());
        assert_eq!(
            mapped
                .source_offsets
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        let mut node = leaf("text", "AXTextArea", Some("α🧪"));
        node.mapped_value = Some(mapped);
        assert_eq!(
            select_node(&node, "α🧪", None, None, Mode::Text).unwrap(),
            TextRange {
                location: 0,
                length: 3
            }
        );
    }
    for (prefix, marker) in [
        ("7.", "7."),
        ("٧.", "7."),
        ("•", "*"),
        ("checklist item, incomplete", "* [ ]"),
        ("checklist item, completed", "* [x]"),
    ] {
        let mapped = rendered(
            "first\nsecond",
            TextStyle {
                list: Some(ListStyle::from_accessibility(1, 7, prefix)),
                ..Default::default()
            },
        );
        assert_eq!(
            mapped.text,
            format!("    {marker} first\n    {marker} second")
        );
        assert_eq!(
            mapped
                .source_offsets
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        rendered(
            "quoted\nagain",
            TextStyle {
                blockquote: 2,
                ..Default::default()
            }
        )
        .text,
        ">> quoted\n>> again"
    );
    assert_eq!(
        rendered(
            "emphasis",
            TextStyle {
                italic: true,
                ..Default::default()
            }
        )
        .text,
        "*emphasis*"
    );
    let mapped = render_attributed(
        "abc",
        &[
            AttributedRun {
                range: TextRange {
                    location: 0,
                    length: 1,
                },
                style: TextStyle {
                    bold: true,
                    ..Default::default()
                },
            },
            AttributedRun {
                range: TextRange {
                    location: 1,
                    length: 1,
                },
                style: TextStyle {
                    bold: true,
                    italic: true,
                    ..Default::default()
                },
            },
            AttributedRun {
                range: TextRange {
                    location: 2,
                    length: 1,
                },
                style: TextStyle {
                    bold: true,
                    ..Default::default()
                },
            },
        ],
    )
    .unwrap();
    assert_eq!(
        mapped.text, "**a*b*c**",
        "Common style is retained across run transitions"
    );
    assert_eq!(
        mapped
            .source_offsets
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}
#[cfg(target_os = "macos")]
#[test]
fn original_swift_context_equality_preserves_nsstring_range_lengths() {
    use skyre::selection::select;
    assert_eq!(
        select("ÅxÅ", "x", Some("Å"), Some("Å"), Mode::Text).unwrap(),
        TextRange {
            location: 1,
            length: 1
        }
    );
    assert!(
        select("e\u{301}x", "x", Some("é"), None, Mode::Text).is_err(),
        "Native context is measured before canonical comparison"
    );
}
fn root(children: Vec<Node>) -> Node {
    Node {
        identity: "root".into(),
        role: "AXWindow".into(),
        title: Some("Fixture".into()),
        children,
        enabled: true,
        ..Default::default()
    }
}
#[test]
fn original_live_native_state_grammar_focus_selection_and_labels() {
    let mut field = leaf("field", "AXTextField", Some("alpha"));
    field.settable = true;
    field.description = Some("Fixture input".into());
    field.identifier = Some("fixture-input".into());
    let mut window = root(vec![field.clone()]);
    window.actions = vec!["AXRaise".into()];
    window.focus_tree = Some(Box::new(field));
    let revision = Revision::root(window);
    let full = skyre::ax::format_state("Fixture", "Owned", &revision.full_text(), &revision);
    assert_eq!(
        full,
        "Window: \"Fixture\", App: Owned.\n0 standard window Fixture, Secondary Actions: Raise\n\t1 text field (settable) Description: Fixture input, Value: alpha, ID: fixture-input\n\nThe focused UI element is 1 text field (settable) Description: Fixture input, Value: alpha, ID: fixture-input"
    );
    let unchanged = skyre::ax::format_state("Fixture", "Owned", "", &revision);
    assert!(unchanged.starts_with("There has been no change in the accessibility tree for Window: \"Fixture\".\nThe focused UI element is 1 "));
    let mut selected = revision.clone();
    selected.focus.as_mut().unwrap().selected_text = Some("alpha".into());
    let state = skyre::ax::format_state("Fixture", "Owned", "", &selected);
    assert!(state.contains(
        "\nSelected text: \x60\x60\x60\nalpha\n\x60\x60\x60\n\nNote: Pay special attention"
    ));
    assert!(!state.contains("The focused UI element"));
    let diff = skyre::ax::format_state("Fixture", "Owned", "~\t1 text field", &revision);
    assert!(diff.starts_with("The following is a diff from the previous accessibility tree for Window: \"Fixture\" with ~ and + representing changed and added elements, respectively. Removed elements are summarized by ID range.\n~\t1 text field\n"));
    assert_eq!(revision.root.action_named("Raise"), Some("AXRaise"));
    assert_eq!(revision.root.action_named("AXRaise"), Some("AXRaise"));
    let mut numeric = leaf("value", "AXValueIndicator", Some("25"));
    numeric.numeric_value = true;
    numeric.settable = true;
    assert_eq!(numeric.text(), "value indicator (settable, float) 25");
    let mut item = leaf("item", "AXMenuItem", None);
    item.title = Some("Mercury".into());
    item.selected = true;
    assert_eq!(item.text(), "(selected) Mercury");
}
#[test]
fn source_mapping_uses_utf16_and_ignores_markup_then_applies_cursor_mode() {
    let mut mapped = MappedText::default();
    mapped.syntax("**");
    mapped.append(&MappedText::plain("🧪bold", 3));
    mapped.syntax("**");
    let mut node = leaf("text", "AXTextArea", Some("abc🧪boldz"));
    node.mapped_value = Some(mapped.clone());
    assert_eq!(
        select_node(&node, "**🧪bold**", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 3,
            length: 6
        }
    );
    assert_eq!(
        select_node(&node, "**🧪bold**", None, None, Mode::CursorAfter).unwrap(),
        TextRange {
            location: 9,
            length: 0
        }
    );
    assert_eq!(
        select_node(&node, "abc", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 0,
            length: 3
        }
    );
    assert!(
        mapped
            .source_range(TextRange {
                location: 0,
                length: 2
            })
            .is_err()
    );
    mapped.source_offsets[4] = Some(900);
    assert!(
        mapped
            .source_range(TextRange {
                location: 2,
                length: 6
            })
            .is_err()
    );
    mapped.source_offsets.pop();
    assert!(
        mapped
            .source_range(TextRange {
                location: 2,
                length: 1
            })
            .is_err()
    );
}
#[test]
fn guarded_transform_restarts_replacement_and_preserves_interaction() {
    let mut group = leaf("group", "AXGroup", None);
    group.children = vec![leaf("empty", "AXGroup", None)];
    let mut field = leaf("field", "AXTextField", None);
    field.focusable = true;
    field.settable = true;
    let mut image = leaf("image", "AXImage", None);
    image.enabled = false;
    let output = transform::apply(
        root(vec![group, field, image]),
        "org.skyre.fixture",
        Context::FullUi,
    );
    assert_eq!(
        output
            .children
            .iter()
            .map(|c| c.identity.as_str())
            .collect::<Vec<_>>(),
        ["field", "image"]
    );
    let mut focused = leaf("focus", "AXGroup", None);
    focused.focused = true;
    assert!(
        transform::apply(root(vec![focused]), "fixture", Context::FullUi)
            .by_identity("focus")
            .is_some()
    );
}
#[test]
fn full_ui_action_filter_respects_widget_and_scrollbar_guards() {
    let mut field = leaf("field", "AXTextField", Some("value"));
    field.settable = true;
    field.actions = vec![
        "AXConfirm".into(),
        "AXIncrement".into(),
        "AXDecrement".into(),
        "AXShowMenu".into(),
    ];
    let mut scroll = leaf("scroll", "AXScrollArea", None);
    scroll.actions = vec!["AXScrollDownByPage".into(), "AXScrollRightByPage".into()];
    scroll.children = vec![leaf("v", "AXScrollBar", None)];
    let output = transform::apply(
        root(vec![field.clone(), scroll]),
        "fixture",
        Context::FullUi,
    );
    assert_eq!(output.by_identity("field").unwrap().actions, ["AXShowMenu"]);
    assert_eq!(
        output.by_identity("scroll").unwrap().actions,
        ["AXScrollDownByPage"]
    );
    let output = transform::apply(root(vec![field]), "fixture", Context::EventStream);
    assert!(
        output
            .by_identity("field")
            .unwrap()
            .actions
            .contains(&"AXConfirm".into())
    );
}
#[test]
fn title_association_exact_identity_and_preserves_provider_title() {
    let mut label = leaf("label", "AXStaticText", Some("Label text"));
    label.title_for = vec!["field".into()];
    let mut field = leaf("field", "AXTextField", Some("v"));
    field.title = Some("Provider title".into());
    let output = transform::apply(root(vec![label, field]), "fixture", Context::FullUi);
    assert!(output.by_identity("label").is_none());
    assert_eq!(
        output.by_identity("field").unwrap().title.as_deref(),
        Some("Provider title")
    );
    let mut ambiguous = leaf("ambiguous", "AXStaticText", Some("label"));
    ambiguous.title_for = vec!["a".into(), "b".into()];
    assert!(
        transform::apply(root(vec![ambiguous]), "fixture", Context::FullUi)
            .by_identity("ambiguous")
            .is_some()
    );
}
#[test]
fn table_selectable_guard_and_calendar_shape_are_specific() {
    let mut table = leaf("table", "AXTable", None);
    table.selectable = true;
    table.children = vec![leaf("row", "AXStaticText", Some("data"))];
    let mut event = leaf("event", "AXStaticText", None);
    event.title = Some("Event".into());
    event.children = vec![leaf("child", "AXStaticText", Some("details"))];
    let normal = transform::apply(root(vec![table, event.clone()]), "fixture", Context::FullUi);
    assert_eq!(normal.by_identity("table").unwrap().children.len(), 1);
    assert!(normal.by_identity("child").is_some());
    assert!(
        transform::apply(root(vec![event]), "com.apple.iCal", Context::FullUi)
            .by_identity("child")
            .is_none()
    );
}
#[test]
fn diff_uses_structure_not_recycled_numeric_ids_and_ignores_detail_only_changes() {
    let old = Revision::root(root(vec![leaf("a", "AXButton", Some("A"))]));
    let new = old
        .append(leaf("replacement", "AXButton", Some("new")))
        .unwrap();
    assert_eq!(new.root.id, Some(1));
    assert!(new.diff(&old).is_none());
    let all = new
        .diff_with_options(
            &old,
            DiffOptions {
                ignore_line_budget: true,
                summarize_removed: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(all.lines().next().unwrap().starts_with("-0"));
    assert!(all.contains("+1 button"));
    let mut node = old.root.clone();
    node.children[0].detail = Some("new detail".into());
    let new = old.append(node).unwrap();
    assert_eq!(new.diff(&old).unwrap(), "");
    assert!(new.full_text().contains("new detail"));
}
#[test]
fn diff_sorted_paths_collapsed_subtrees_and_summary_budget() {
    let old = Revision::root(root(vec![
        leaf("a", "AXButton", Some("old")),
        leaf("b", "AXButton", Some("keep")),
    ]));
    let mut inserted = leaf("insert", "AXGroup", Some("group"));
    inserted.children = vec![leaf("kid", "AXStaticText", Some("child"))];
    let new = old
        .append(root(vec![inserted, leaf("b", "AXButton", Some("changed"))]))
        .unwrap();
    let diff = new
        .diff_with_options(
            &old,
            DiffOptions {
                summarize_removed: false,
                ignore_line_budget: true,
                expand_inserts: false,
                expand_removals: false,
                include_ids: false,
                ..Default::default()
            },
        )
        .unwrap();
    let lines: Vec<_> = diff.lines().collect();
    assert!(lines[0].starts_with("-\tbutton"));
    assert!(lines[1].starts_with("+\tcontainer"));
    assert!(lines[2].starts_with("~\tbutton"));
    assert_eq!(lines.len(), 3);
}
#[cfg(target_os = "macos")]
#[test]
fn native_key_aliases_and_modifiers() {
    use skyre::native::keys::parse;
    assert_eq!(parse("cmd+C").unwrap()[0].code, 8);
    assert_eq!(parse("ctrl+shift+Left").unwrap()[0].code, 123);
    assert!(parse("hyper+x").is_err());
    assert!(parse("unknown").is_err());
}
#[test]
fn attributed_text_styles_keep_source_offsets_and_reject_invalid_runs() {
    use skyre::rich_text::{AttributedRun, TextStyle, render_attributed};
    let mapped = render_attributed(
        "A🧪boldZ",
        &[AttributedRun {
            range: TextRange {
                location: 1,
                length: 6,
            },
            style: TextStyle {
                bold: true,
                underline: true,
                link: Some("https://example.invalid/(local)".into()),
                ..Default::default()
            },
        }],
    )
    .unwrap();
    assert!(
        mapped
            .text
            .contains("**<u>[🧪bold](https://example.invalid/(local))</u>**")
    );
    assert_eq!(
        mapped.source_offsets.len(),
        mapped.text.encode_utf16().count()
    );
    let mut node = leaf("text", "AXTextArea", Some("A🧪boldZ"));
    node.mapped_value = Some(mapped);
    assert_eq!(
        select_node(&node, "🧪bold", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 1,
            length: 6
        }
    );
    for range in [
        TextRange {
            location: 2,
            length: 1,
        },
        TextRange {
            location: 9,
            length: 1,
        },
        TextRange {
            location: usize::MAX,
            length: 2,
        },
    ] {
        assert!(
            render_attributed(
                "A🧪boldZ",
                &[AttributedRun {
                    range,
                    style: TextStyle::default()
                }]
            )
            .is_err()
        );
    }
    assert!(
        render_attributed(
            "abc",
            &[
                AttributedRun {
                    range: TextRange {
                        location: 0,
                        length: 2
                    },
                    style: TextStyle::default()
                },
                AttributedRun {
                    range: TextRange {
                        location: 1,
                        length: 2
                    },
                    style: TextStyle::default()
                }
            ]
        )
        .is_err()
    );
}
#[cfg(target_os = "macos")]
#[test]
fn foundation_canonical_selection_returns_original_utf16_range() {
    use skyre::selection::select;
    assert_eq!(
        select("A e\u{301} Z", "é", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 2,
            length: 2
        }
    );
    assert_eq!(
        select("A é Z", "e\u{301}", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 2,
            length: 1
        }
    );
    assert!(select("e\u{301}é", "é", None, None, Mode::Text).is_err());
}
#[test]
fn focus_view_shares_source_ids_and_allocates_after_main_maximum() {
    let main = leaf("field", "AXTextField", Some("value"));
    let mut tree = root(vec![main.clone()]);
    tree.focus_tree = Some(Box::new(main));
    let revision = Revision::root(tree);
    assert_eq!(revision.root.children[0].id, Some(1));
    assert_eq!(revision.focus.as_ref().unwrap().id, Some(1));
    assert!(revision.root.focus_tree.is_none());
    let mut next = root(vec![leaf("field", "AXTextField", Some("value"))]);
    next.focus_tree = Some(Box::new(leaf(
        "detached-focus",
        "AXTextField",
        Some("outside rendered main"),
    )));
    let revision = revision.append(next).unwrap();
    let focus = revision.focus.as_ref().unwrap();
    assert_eq!(focus.id, Some(2));
    assert_eq!(revision.by_id(2).unwrap().identity, "detached-focus");
    assert!(
        revision
            .focus_text()
            .unwrap()
            .contains("outside rendered main")
    );
}
#[test]
fn focus_view_does_not_create_false_semantic_ambiguity_on_refetch() {
    use skyre::ax::Sessions;
    let field = leaf("field", "AXTextField", Some("value"));
    let mut tree = root(vec![field.clone()]);
    tree.focus_tree = Some(Box::new(field));
    let mut sessions = Sessions::default();
    sessions.observe("fixture", tree.clone(), false).unwrap();
    tree.children[0].identity = "replacement".into();
    tree.focus_tree = Some(Box::new(tree.children[0].clone()));
    assert_eq!(
        sessions
            .resolve("fixture", 1, tree, false)
            .unwrap()
            .identity,
        "replacement"
    );
}

#[test]
fn original_list_marker_keeps_number_string_value_separate_from_integer_identity() {
    use skyre::rich_text::ListStyle;
    let style = ListStyle::from_accessibility_index_label(1, 3, "3.5", "3.5.");
    assert_eq!(style.index, 3);
    assert_eq!(style.marker, "3.5.");
    assert_eq!(
        ListStyle::from_accessibility_index_label(1, 3, "3.5", "•").marker,
        "*"
    );
}
