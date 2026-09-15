//! Source-grounded expectations from native-sibling-merge/20260907T011627454559Z.
//! The vendor executable is never run by these tests.
use skyre::{
    ax::{
        Node,
        transform::{self, Context},
    },
    native::{
        render::{self, SemanticText},
        url::{Configuration, Shortener},
    },
    selection::{self, MappedText, Mode, TextRange},
};

const CONTEXTS: [Context; 3] = [
    Context::FullUi,
    Context::EventStream,
    Context::Informational,
];

fn text(id: &str, value: Option<&str>) -> Node {
    Node {
        identity: id.into(),
        role: "AXStaticText".into(),
        value: value.map(str::to_owned),
        ..Default::default()
    }
}
fn parent(children: Vec<Node>) -> Node {
    Node {
        identity: "parent".into(),
        role: "AXWindow".into(),
        title: Some("Owned fixture".into()),
        children,
        ..Default::default()
    }
}
fn link(id: &str, value: Option<&str>, url: Option<&str>) -> Node {
    Node {
        role: "AXLink".into(),
        url: url.map(str::to_owned),
        ..text(id, value)
    }
}
fn merge(node: &mut Node, context: Context) {
    transform::merge_text_only_siblings(node, context, &mut Shortener::default());
}

#[test]
fn sibling_static_merge_runs_in_all_contexts_and_the_public_full_ui_pipeline() {
    for context in CONTEXTS {
        let mut node = parent(vec![
            text("first", Some("alpha")),
            text("second", Some("beta")),
        ]);
        merge(&mut node, context);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].identity, "first");
        assert_eq!(node.children[0].value.as_deref(), Some("alpha beta"));
        assert_eq!(node.children[0].role_description.as_deref(), Some("text"));
    }
    let node = render::prepare_full_ui(
        parent(vec![
            text("first", Some("alpha")),
            text("second", Some("beta")),
        ]),
        None,
        "owned",
        &mut Shortener::default(),
        &mut |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].text(), "text alpha beta");
}

#[test]
fn sibling_parent_focusable_and_selectable_each_gate_all_contexts() {
    for context in CONTEXTS {
        for selectable in [false, true] {
            let mut node = parent(vec![
                text("first", Some("alpha")),
                text("second", Some("beta")),
            ]);
            node.selectable = selectable;
            node.focusable = !selectable;
            let before = node.clone();
            merge(&mut node, context);
            assert_eq!(node, before);
        }
        let mut node = parent(vec![
            text("first", Some("alpha")),
            text("second", Some("beta")),
        ]);
        node.focused = true;
        merge(&mut node, context);
        assert_eq!(
            node.children.len(),
            1,
            "focused alone is not the source gate"
        );
    }
}

#[test]
fn sibling_runs_distinguish_missing_values_and_singletons_from_empty_values() {
    let mut node = parent(vec![
        text("a", Some("a")),
        text("b", Some("b")),
        text("missing", None),
        text("empty", Some("")),
        text("c", Some("c")),
        Node {
            role: "AXButton".into(),
            ..text("button", Some("button"))
        },
        text("solo", Some("solo")),
    ]);
    merge(&mut node, Context::FullUi);
    assert_eq!(
        node.children
            .iter()
            .map(|n| n.identity.as_str())
            .collect::<Vec<_>>(),
        ["a", "missing", "empty", "button", "solo"]
    );
    assert_eq!(node.children[0].value.as_deref(), Some("a b"));
    assert_eq!(node.children[2].value.as_deref(), Some("c"));
    assert_eq!(node.children[4], text("solo", Some("solo")));
}

#[test]
fn sibling_whitespace_filter_preserves_nonempty_surrounding_text_and_first_identity() {
    for (values, expected) in [
        (["  alpha ", " beta\n"], "  alpha   beta\n"),
        (["\t\u{200b}\u{0085}", "β🧪"], "β🧪"),
        (["\r\n\u{00a0}", "\u{200b}"], ""),
        (["\u{feff}", "x"], "\u{feff} x"),
    ] {
        let mut node = parent(vec![
            text("first", Some(values[0])),
            text("second", Some(values[1])),
        ]);
        merge(&mut node, Context::FullUi);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].identity, "first");
        assert_eq!(node.children[0].value.as_deref(), Some(expected));
        assert_eq!(
            node.children[0].attributed_source.as_deref(),
            Some(values[0])
        );
    }
}

#[test]
fn sibling_later_text_and_context_never_become_first_source_offsets() {
    let mut node = parent(vec![
        text("first", Some("same")),
        text("later", Some("prefix same suffix")),
    ]);
    merge(&mut node, Context::FullUi);
    let first = &node.children[0];
    assert_eq!(
        selection::select_node(first, "same", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 0,
            length: 4
        }
    );
    assert!(
        selection::select_node(first, "same", Some("prefix "), Some(" suffix"), Mode::Text)
            .is_err()
    );
    assert!(selection::select_node(first, "prefix", None, None, Mode::Text).is_err());
    assert!(selection::select_node(first, "same prefix", None, None, Mode::Text).is_err());
}

#[test]
fn sibling_preserves_first_raw_attributed_source_unicode_and_once_only_origin() {
    let mut first = text("first", Some("# é🧪"));
    first.attributed_source = Some("\u{feff}é🧪".into());
    first.truncation_range = Some(TextRange {
        location: 17,
        length: 4,
    });
    first.mapped_value = Some(MappedText::plain("# é🧪", 90));
    first.semantic_value = Some(SemanticText::url(
        "https://example.test/stale".into(),
        false,
        "stale".into(),
    ));
    first.prepared_attributes = Some("stale attributes".into());
    let mut node = parent(vec![first, text("later", Some("ONLY🧪"))]);
    merge(&mut node, Context::FullUi);
    let first = &node.children[0];
    assert!(
        first.mapped_value.is_none()
            && first.semantic_value.is_none()
            && first.prepared_attributes.is_none()
    );
    assert_eq!(
        selection::select_node(first, "é🧪", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 18,
            length: 3
        }
    );
    assert_eq!(
        selection::select_node(first, "é🧪", None, None, Mode::CursorAfter).unwrap(),
        TextRange {
            location: 21,
            length: 0
        }
    );
    assert!(selection::select_node(first, "#", None, None, Mode::Text).is_err());
    assert!(selection::select_node(first, "ONLY", None, None, Mode::Text).is_err());
}

#[test]
fn sibling_keeps_first_action_identity_without_an_unproven_editable_candidate_filter() {
    let mut first = text("first", Some("one"));
    first.focusable = true;
    first.selectable = true;
    first.settable = true;
    first.actions = vec!["AXPress".into(), "AXShowMenu".into()];
    first.children.push(text("grandchild", Some("hidden")));
    first.id = Some(42);
    let mut later = text("second", Some("two"));
    later.focusable = true;
    let mut node = parent(vec![first, later]);
    merge(&mut node, Context::FullUi);
    let first = &node.children[0];
    assert_eq!(first.id, Some(42));
    assert_eq!(first.identity, "first");
    assert!(first.focusable && first.selectable && first.settable);
    assert_eq!(first.actions, ["AXPress", "AXShowMenu"]);
    assert!(first.children.is_empty());
    assert!(selection::select_node(first, "two", None, None, Mode::Text).is_err());
}

#[test]
fn sibling_links_are_informational_only_and_use_recovered_label_priority() {
    for context in CONTEXTS {
        let mut labeled = link("link", Some(" [one] "), Some("https://www.example.test/x"));
        labeled.title = Some("unused title".into());
        let mut node = parent(vec![text("first", Some("lead")), labeled]);
        merge(&mut node, context);
        if context == Context::Informational {
            assert_eq!(node.children.len(), 1);
            assert_eq!(
                node.children[0].value.as_deref(),
                Some("lead [\\[one\\]](example.test/x)")
            );
        } else {
            assert_eq!(node.children.len(), 2);
        }
    }
    let mut whitespace = link("link", Some(" \n "), Some("https://example.test/x"));
    whitespace.title = Some("must not replace nonempty whitespace".into());
    let mut node = parent(vec![text("first", Some("lead")), whitespace]);
    merge(&mut node, Context::Informational);
    assert_eq!(
        node.children[0].value.as_deref(),
        Some("lead [](example.test/x)")
    );
}

#[test]
fn sibling_link_missing_url_empty_label_and_title_description_fallbacks() {
    for (value, title, description, expected) in [
        (
            None,
            Some("title"),
            Some("description"),
            "lead [title](example.test)",
        ),
        (
            Some(""),
            Some(""),
            Some("description"),
            "lead [description](example.test)",
        ),
        (None, None, None, "lead <example.test>"),
    ] {
        let mut candidate = link("link", value, Some("https://example.test"));
        candidate.title = title.map(str::to_owned);
        candidate.description = description.map(str::to_owned);
        let mut node = parent(vec![text("first", Some("lead")), candidate]);
        merge(&mut node, Context::Informational);
        assert_eq!(node.children[0].value.as_deref(), Some(expected));
    }
    let mut node = parent(vec![
        text("first", Some("lead")),
        link("link", Some("ignored"), None),
    ]);
    merge(&mut node, Context::Informational);
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].value.as_deref(), Some("lead"));
}

#[test]
fn sibling_link_led_run_is_skipped_but_candidate_url_bookkeeping_still_happens() {
    let mut node = parent(vec![
        link("link", None, Some("https://example.test/x")),
        text("one", Some("one")),
        text("two", Some("two")),
    ]);
    let before = node.clone();
    let mut urls = Shortener::default();
    transform::merge_text_only_siblings(&mut node, Context::Informational, &mut urls);
    assert_eq!(node, before);
    assert_eq!(urls.total(), "example.test/x".len());
}

#[test]
fn sibling_later_url_interpolation_is_flattened_and_not_retrimmed() {
    let mut node = parent(vec![
        text("first", Some("lead")),
        link("link", None, Some("https://example.test/long")),
    ]);
    let mut urls = Shortener::new(Configuration {
        individual_limit: Some(8),
        ..Default::default()
    })
    .unwrap();
    transform::merge_text_only_siblings(&mut node, Context::Informational, &mut urls);
    assert_eq!(node.children[0].value.as_deref(), Some("lead <example…>"));
    assert!(node.children[0].semantic_value.is_none());
    assert!(selection::select_node(&node.children[0], "example", None, None, Mode::Text).is_err());
}

#[test]
fn sibling_join_cannot_bypass_the_final_native_output_limit() {
    let first = "a".repeat(2 * 1024 * 1024);
    let second = "b".repeat(2 * 1024 * 1024);
    let error = render::prepare_full_ui(
        parent(vec![
            text("first", Some(&first)),
            text("second", Some(&second)),
        ]),
        None,
        "owned",
        &mut Shortener::default(),
        &mut |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(
        error.message.contains("rendering bound"),
        "{}",
        error.message
    );
}
