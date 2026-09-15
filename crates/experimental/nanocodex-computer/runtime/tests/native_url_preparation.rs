//! Expected branches come from the two immutable native-url-compaction captures,
//! not execution of the vendor service. These are replacement conformance tests.
use skyre::{
    Error,
    ax::{Node, Revision},
    native::{
        render::{self, AttributedInput, DeferredRun, Part, SemanticText},
        url::{Configuration, Shortener},
    },
    rich_text::{AttributedRun, TextStyle},
    selection::{MappedText, TextRange},
};

fn node(id: &str, role: &str, value: Option<&str>) -> Node {
    Node {
        identity: id.into(),
        role: role.into(),
        value: value.map(str::to_owned),
        ..Default::default()
    }
}
fn window(children: Vec<Node>) -> Node {
    Node {
        children,
        title: Some("Owned fixture".into()),
        ..node("window", "AXWindow", None)
    }
}
fn link(id: &str, url: &str) -> Node {
    Node {
        url: Some(url.into()),
        focusable: true,
        ..node(id, "AXLink", None)
    }
}
fn input(url: &str) -> AttributedInput {
    AttributedInput {
        source: "label".into(),
        runs: vec![DeferredRun {
            run: AttributedRun {
                range: TextRange {
                    location: 0,
                    length: 5,
                },
                style: TextStyle::default(),
            },
            link: Some((url.into(), false)),
            attachment_url: None,
        }],
    }
}
fn prepare(root: Node, focus: Option<Box<Node>>, urls: &mut Shortener) -> Node {
    render::prepare_full_ui(root, focus, "owned", urls, &mut |_, _| Ok(())).unwrap()
}

#[test]
fn semantic_retrim_changes_only_url_parts_and_rebuilds_generated_utf16_offsets() {
    let literal = "\u{feff}é🧪 example.com/long";
    let mut semantic = SemanticText {
        parts: vec![
            Part::Literal {
                mapped: MappedText::plain(literal, 7),
            },
            Part::Url {
                original: "https://example.com/long".into(),
                is_image: false,
                display: "example.com/long".into(),
            },
        ],
    };
    let mut urls = Shortener::new(Configuration {
        individual_limit: Some(8),
        ..Default::default()
    })
    .unwrap();
    assert!(semantic.retrim(&mut urls).unwrap());
    let mapped = semantic.mapped().unwrap();
    assert_eq!(mapped.text, format!("{literal}example…"));
    let n = literal.encode_utf16().count();
    assert_eq!(
        &mapped.source_offsets[..n],
        &(7..7 + n).map(Some).collect::<Vec<_>>()
    );
    assert_eq!(&mapped.source_offsets[n..], &[None; 8]);
    assert_eq!(
        mapped.source_offsets.len(),
        mapped.text.encode_utf16().count()
    );
}

#[test]
fn semantic_unchanged_and_suppressed_branches_preserve_original_url_metadata() {
    let mut same = SemanticText::url(
        "https://example.com/a".into(),
        false,
        "example.com/a".into(),
    );
    assert!(!same.retrim(&mut Shortener::default()).unwrap());
    let mut image = SemanticText::url("https://example.com/a".into(), true, "old".into());
    assert!(image.retrim(&mut Shortener::default()).unwrap());
    assert_eq!(image.mapped().unwrap().text, "");
    assert!(
        matches!(&image.parts[0], Part::Url { original, is_image: true, display } if original == "https://example.com/a" && display.is_empty())
    );
}

#[test]
fn semantic_invalid_source_maps_parts_and_raw_url_bounds_are_errors() {
    let invalid = SemanticText {
        parts: vec![Part::Literal {
            mapped: MappedText {
                text: "🧪".into(),
                source_offsets: vec![Some(0)],
            },
        }],
    };
    assert!(invalid.mapped().unwrap_err().message.contains("source map"));
    let excessive = SemanticText {
        parts: vec![
            Part::Literal {
                mapped: MappedText::default()
            };
            65537
        ],
    };
    assert!(
        excessive
            .mapped()
            .unwrap_err()
            .message
            .contains("part bound")
    );
    let excessive = SemanticText::url("x".repeat(1024 * 1024 + 1), false, "".into());
    assert!(
        excessive
            .mapped()
            .unwrap_err()
            .message
            .contains("length bound")
    );
}

#[test]
fn raw_url_is_shortened_before_dedup_and_sole_url_value_has_no_label() {
    let mut urls = Shortener::default();
    let mut control = node("control", "AXButton", None);
    control.url = Some("https://www.example.com/a".into());
    assert_eq!(
        render::attributes(&control, &mut urls).unwrap(),
        "example.com/a"
    );
    assert_eq!(urls.total(), 13);
    control.title = Some("example.com/a".into());
    assert_eq!(
        render::attributes(&control, &mut urls).unwrap(),
        "example.com/a"
    );
    assert_eq!(
        urls.total(),
        26,
        "deduplicated URL still consumes its display count"
    );
    control.title = Some("Open".into());
    control.help = Some("Hint".into());
    assert_eq!(
        render::attributes(&control, &mut urls).unwrap(),
        "Open, URL: example.com/a, Help: Hint"
    );
    assert_eq!(control.url.as_deref(), Some("https://www.example.com/a"));
}

#[test]
fn raw_url_image_suppression_and_long_attribute_order_match_recovered_branches() {
    let mut control = node("image", "AXImage", None);
    control.url = Some("https://example.com/image".into());
    assert_eq!(
        render::attributes(&control, &mut Shortener::default()).unwrap(),
        ""
    );
    control.role = "AXButton".into();
    control.title = Some("Open".into());
    let raw = format!("https://example.com/{}", "x".repeat(101));
    control.url = Some(raw.clone());
    control.help = Some("Hint".into());
    assert_eq!(
        render::attributes(&control, &mut Shortener::default()).unwrap(),
        format!(
            "Open, Help: Hint, URL: {}",
            raw.trim_start_matches("https://")
        )
    );
    control.identifier = Some("NSInternal".into());
    assert!(
        !render::attributes(&control, &mut Shortener::default())
            .unwrap()
            .contains("NSInternal")
    );
}

#[test]
fn full_ui_late_attributed_callback_visits_only_retained_nodes_in_main_then_focus_order() {
    let retained = node("main-text", "AXTextArea", Some("main"));
    let folded = Node {
        selectable: true,
        description: Some("row".into()),
        children: vec![node("folded-text", "AXTextArea", Some("folded"))],
        ..node("row", "AXRow", None)
    };
    let empty_disabled = Node {
        enabled: false,
        ..node("disabled-text", "AXTextArea", None)
    };
    let focus = node("focus-text", "AXTextArea", Some("focus"));
    let mut calls = Vec::new();
    let result = render::prepare_full_ui(
        window(vec![retained, folded, empty_disabled]),
        Some(Box::new(focus)),
        "owned",
        &mut Shortener::default(),
        &mut |node, _| {
            calls.push(node.identity.clone());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls, ["main-text", "focus-text"]);
    assert!(result.by_identity("folded-text").is_none());
    assert!(result.by_identity("disabled-text").is_none());
    assert!(result.focus_tree.unwrap().prepared_attributes.is_some());
}

#[test]
fn raw_control_urls_drive_compaction_and_pruned_urls_do_not() {
    let raw = format!("https://example.com/a?{}", "q".repeat(86));
    let control = Node {
        url: Some(raw.clone()),
        ..node("control", "AXButton", None)
    };
    let other = Node {
        url: Some(format!("https://example.com/b?{}", "q".repeat(86))),
        ..node("other", "AXButton", None)
    };
    let mut urls = Shortener::new(Configuration {
        total_limit: Some(100),
        ..Default::default()
    })
    .unwrap();
    let output = prepare(window(vec![control, other]), None, &mut urls);
    assert!(!urls.configuration().include_query);
    let control = output.by_identity("control").unwrap();
    assert_eq!(control.url.as_ref(), Some(&raw));
    assert_eq!(control.text(), "button example.com/a");
    let pruned = Node {
        url: Some(raw),
        ..node("pruned", "AXGroup", None)
    };
    let mut urls = Shortener::default();
    let output = prepare(window(vec![pruned]), None, &mut urls);
    assert!(output.by_identity("pruned").is_none());
    assert!(urls.configuration().include_query);
    assert_eq!(urls.total(), 0);
}

#[test]
fn semantic_main_focus_and_final_raw_url_render_share_the_recovered_counter_order() {
    let mut urls = Shortener::new(Configuration {
        total_limit: Some(13),
        ..Default::default()
    })
    .unwrap();
    let raw = Node {
        url: Some("https://example.com/c".into()),
        ..node("raw", "AXButton", None)
    };
    let output = prepare(
        window(vec![link("main", "https://example.com/a"), raw]),
        Some(Box::new(link("focus", "https://example.com/b"))),
        &mut urls,
    );
    assert_eq!(
        output.by_identity("main").unwrap().value.as_deref(),
        Some("example.com/a")
    );
    assert_eq!(
        output.focus_tree.as_ref().unwrap().value.as_deref(),
        Some("…")
    );
    assert_eq!(
        output
            .by_identity("raw")
            .unwrap()
            .prepared_attributes
            .as_deref(),
        Some("…")
    );
    assert_eq!(
        urls.total(),
        15,
        "13 main + 1 focus retrim + 1 final raw attribute"
    );
}

#[test]
fn attributed_markdown_destination_remains_literal_when_semantic_urls_are_retrimmed() {
    let raw = format!("https://example.com/a?{}", "q".repeat(86));
    let other = format!("https://example.com/b?{}", "q".repeat(86));
    let source = input(&raw);
    let mut calls = 0;
    let output = render::prepare_full_ui(
        window(vec![
            node("field", "AXTextArea", Some("label")),
            link("link", &other),
        ]),
        None,
        "owned",
        &mut Shortener::new(Configuration {
            total_limit: Some(100),
            ..Default::default()
        })
        .unwrap(),
        &mut |node, urls| {
            calls += 1;
            let mapped = source.render(urls)?;
            node.value = Some(mapped.text.clone());
            node.mapped_value = Some(mapped);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        calls, 1,
        "compaction does not invoke attributed retrieval again"
    );
    let field = output.by_identity("field").unwrap();
    assert_eq!(
        field.value.as_deref(),
        Some(format!("[label]({})", raw.trim_start_matches("https://")).as_str())
    );
    assert!(field.semantic_value.is_none());
    assert_eq!(
        output.by_identity("link").unwrap().value.as_deref(),
        Some("example.com/b")
    );
    let map = field.mapped_value.as_ref().unwrap();
    assert_eq!(
        &map.source_offsets[1..6],
        &[Some(0), Some(1), Some(2), Some(3), Some(4)]
    );
}

#[test]
fn attributed_callback_failures_abort_preparation_and_skip_remaining_nodes() {
    let mut calls = Vec::new();
    let error = render::prepare_full_ui(
        window(vec![
            node("first", "AXTextArea", Some("x")),
            node("second", "AXTextArea", Some("y")),
        ]),
        None,
        "owned",
        &mut Shortener::default(),
        &mut |node, _| {
            calls.push(node.identity.clone());
            Err(Error::action("owned provider denied"))
        },
    )
    .unwrap_err();
    assert_eq!(error.message, "owned provider denied");
    assert_eq!(calls, ["first"]);
}

#[test]
fn prepared_revision_formatting_and_diffing_do_not_consume_url_budget_again() {
    let mut urls = Shortener::default();
    let control = Node {
        url: Some("https://example.com/a".into()),
        ..node("control", "AXButton", None)
    };
    let output = prepare(window(vec![control]), None, &mut urls);
    let total = urls.total();
    let revision = Revision::root(output.clone());
    let same = revision.append(output).unwrap();
    assert_eq!(same.diff(&revision).as_deref(), Some(""));
    for _ in 0..5 {
        assert!(revision.full_text().contains("button example.com/a"));
    }
    assert_eq!(urls.total(), total);
}

#[test]
fn url_retention_and_tree_limits_fail_without_publishing_partial_preparation() {
    let large = Node {
        url: Some("x".repeat(1024 * 1024 + 1)),
        ..node("control", "AXButton", None)
    };
    let error = render::prepare_full_ui(
        window(vec![large]),
        None,
        "owned",
        &mut Shortener::default(),
        &mut |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(error.message.contains("retained-input bounds"));
    let mut deep = node("leaf", "AXButton", None);
    for i in 0..100 {
        deep = Node {
            children: vec![deep],
            ..node(&format!("{i}"), "AXWindow", Some("keep"))
        };
    }
    let error = render::prepare_full_ui(
        deep,
        None,
        "owned",
        &mut Shortener::default(),
        &mut |_, _| panic!("must validate before callbacks"),
    )
    .unwrap_err();
    assert!(error.message.contains("tree bound"));
}

#[cfg(target_os = "macos")]
#[test]
fn full_ui_attribute_dedup_uses_swift_canonical_string_equality() {
    let control = Node {
        title: Some("e\u{301}".into()),
        description: Some("é".into()),
        ..node("control", "AXButton", None)
    };
    assert_eq!(
        render::attributes(&control, &mut Shortener::default()).unwrap(),
        "e\u{301}"
    );
}

#[test]
fn sole_attribute_enforces_utf16_bound_before_early_bare_value_return() {
    let oversized = Node {
        title: Some("x".repeat(4 * 1024 * 1024 + 1)),
        ..node("control", "AXButton", None)
    };
    assert!(
        render::attributes(&oversized, &mut Shortener::default())
            .unwrap_err()
            .message
            .contains("rendering bound")
    );
    let exact = Node {
        title: Some("🧪".repeat(2 * 1024 * 1024)),
        ..node("control", "AXButton", None)
    };
    assert_eq!(
        render::attributes(&exact, &mut Shortener::default())
            .unwrap()
            .encode_utf16()
            .count(),
        4 * 1024 * 1024
    );
}

#[cfg(target_os = "macos")]
#[test]
fn present_empty_shortened_url_is_not_treated_as_absent() {
    let control = Node {
        title: Some("Open".into()),
        url: Some("https:".into()),
        ..node("control", "AXButton", None)
    };
    assert_eq!(
        render::attributes(&control, &mut Shortener::default()).unwrap(),
        "Open, URL: "
    );
}

#[test]
fn a_single_oversized_url_estimates_as_one_ellipsis_without_query_tightening() {
    let raw = format!("https://example.com/a?{}", "q".repeat(5000));
    let control = Node {
        url: Some(raw),
        ..node("control", "AXButton", None)
    };
    let mut urls = Shortener::default();
    let output = prepare(window(vec![control]), None, &mut urls);
    assert!(urls.configuration().include_query);
    assert_eq!(
        output
            .by_identity("control")
            .unwrap()
            .prepared_attributes
            .as_deref(),
        Some("…")
    );
    assert_eq!(urls.total(), 1);
}
