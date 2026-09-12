//! Independent implementation of the recovered parent-first transformation order.
//! Parent and candidate gates follow the recovered stages. Merged text retains
//! the first native identity and its source, without inventing sibling offsets.
use crate::{ax::Node, selection::MappedText};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Context {
    #[default]
    FullUi,
    EventStream,
    Informational,
}
pub const ORDER: [&str; 13] = [
    "filterActions",
    "associateTitleUIElements",
    "flattenIntoSelectableAncestor",
    "pruneNonDescriptiveSubtrees",
    "pruneEmptyDisabledElements",
    "mergeSingleItemGroups",
    "flattenRedundantHierarchy",
    "flattenRepetitiveStaticText",
    "flattenLinksIntoMarkdownText",
    "mergeTextOnlySiblings",
    "removeElementsUnderCalendarEvents",
    "retrieveAttributedTextFromTextAreas",
    "retrieveAttributedTextFromWebAreas",
];
fn descriptive(n: &Node) -> bool {
    [&n.description, &n.value, &n.title, &n.placeholder]
        .into_iter()
        .any(|s| s.as_deref().is_some_and(|s| !s.trim().is_empty()))
        || n.role == "AXImage"
}
fn interactive(n: &Node) -> bool {
    n.focusable
        || n.selectable
        || n.settable
        || n.focused
        || !n.actions.is_empty()
        || matches!(
            n.role.as_str(),
            "AXButton"
                | "AXCheckBox"
                | "AXRadioButton"
                | "AXMenuItem"
                | "AXMenuBarItem"
                | "AXPopUpButton"
                | "AXComboBox"
                | "AXTextField"
                | "AXTextArea"
                | "AXSlider"
        )
}
fn all(n: &Node, predicate: &impl Fn(&Node) -> bool) -> bool {
    predicate(n) && n.children.iter().all(|c| all(c, predicate))
}
fn display(n: &Node) -> Option<&str> {
    n.value
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(n.description.as_deref().filter(|s| !s.trim().is_empty()))
        .or(n.title.as_deref().filter(|s| !s.trim().is_empty()))
}
fn filtered_actions(n: &mut Node, context: Context) {
    if context != Context::FullUi {
        return;
    }
    let role = &n.role;
    let settable = n.settable;
    let vertical = n
        .children
        .iter()
        .any(|n| n.role == "AXScrollBar" && n.subrole.as_deref() != Some("AXHorizontalScrollBar"));
    let horizontal = n
        .children
        .iter()
        .any(|n| n.role == "AXScrollBar" && n.subrole.as_deref() == Some("AXHorizontalScrollBar"));
    n.actions.retain(|a| match a.as_str() {
        "AXPress" | "AXShowAlternateUI" | "AXShowDefaultUI" => false,
        "AXPick" if role == "AXMenuItem" => false,
        "AXCancel" if matches!(role.as_str(), "AXMenuBar" | "AXMenuItem") => false,
        "AXConfirm" if role == "AXTextField" => false,
        "AXIncrement" | "AXDecrement" if settable => false,
        "AXScrollUpByPage" | "AXScrollDownByPage" => vertical,
        "AXScrollLeftByPage" | "AXScrollRightByPage" => horizontal,
        _ => true,
    });
}

fn trimmed_native(text: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        crate::selection::native_string(text)
            .stringByTrimmingCharactersInSet(
                &objc2_foundation::NSCharacterSet::whitespaceAndNewlineCharacterSet(),
            )
            .to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        text.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{200b}')
            .to_owned()
    }
}

fn sibling_text(
    node: &Node,
    context: Context,
    urls: &mut crate::native::url::Shortener,
) -> Option<String> {
    if node.role == "AXStaticText" {
        return node.value.clone();
    }
    if node.role != "AXLink" || context != Context::Informational {
        return None;
    }
    let Some(url) = node.url.as_deref() else {
        return Some(String::new());
    };
    let label = [
        node.value.as_deref(),
        node.title.as_deref(),
        node.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find(|text| !text.is_empty());
    let destination = urls.trim(url, false).unwrap_or_default();
    Some(match label {
        Some(label) => {
            let escaped = label.replace('[', "\\[").replace(']', "\\]");
            format!("[{}]({destination})", trimmed_native(&escaped))
        }
        None => format!("<{destination}>"),
    })
}

/// One recovered parent-first stage (10063de98 / 100642cf0). The Mac role table
/// supplies a flattened role only for static text among the eligible candidates.
/// No editable-child exclusion is added: original identity/actions stay on the
/// first node, while every later source is display-only. Collection and final
/// rendering budgets are enforced by the surrounding observation preparation.
pub fn merge_text_only_siblings(
    node: &mut Node,
    context: Context,
    urls: &mut crate::native::url::Shortener,
) {
    if node.selectable || node.focusable {
        return;
    }
    // Candidate construction, including URL bookkeeping, precedes all changes.
    let texts: Vec<_> = node
        .children
        .iter()
        .map(|child| sibling_text(child, context, urls))
        .collect();
    let mut runs = Vec::new();
    let mut index = 0;
    while index < texts.len() {
        if texts[index].is_none() {
            index += 1;
            continue;
        }
        let start = index;
        while index < texts.len() && texts[index].is_some() {
            index += 1;
        }
        if index - start > 1 {
            runs.push(start..index);
        }
    }
    for run in runs.into_iter().rev() {
        if node.children[run.start].role != "AXStaticText" {
            continue;
        }
        let value = texts[run.clone()]
            .iter()
            .filter_map(Option::as_deref)
            .filter(|text| !trimmed_native(text).is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let first = &mut node.children[run.start];
        // In the original these fresh candidate literals have no source map.
        // Our fallback reads a retained raw value, never the newly joined text.
        if first.attributed_source.is_none() {
            first.attributed_source = first.value.clone();
        }
        first.value = Some(value);
        first.role_description = Some("text".into());
        first.mapped_value = None;
        first.semantic_value = None;
        first.prepared_attributes = None;
        first.children.clear();
        node.children.drain(run.start + 1..run.end);
    }
}
/// Main roots survive even if the transform would prune an empty leaf. Child
/// replacement restarts the ordered pipeline and is bounded by decreasing depth.
pub fn apply(root: Node, app_id: &str, context: Context) -> Node {
    apply_with_urls(
        root,
        app_id,
        context,
        &mut crate::native::url::Shortener::default(),
    )
}
pub fn apply_with_urls(
    root: Node,
    app_id: &str,
    context: Context,
    urls: &mut crate::native::url::Shortener,
) -> Node {
    let result = apply_with_attributed(root, app_id, context, urls, &mut |_, _| {
        Ok::<_, std::convert::Infallible>(())
    });
    match result {
        Ok(node) => node,
        Err(never) => match never {},
    }
}
/// The late callback runs only on retained text areas after the structural
/// transformations. Failure aborts preparation; it is never an empty AX result.
pub fn apply_with_attributed<E>(
    mut root: Node,
    app_id: &str,
    context: Context,
    urls: &mut crate::native::url::Shortener,
    attributed: &mut impl FnMut(
        &mut Node,
        &mut crate::native::url::Shortener,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<Node, E> {
    let backup = root.clone();
    root = transform(root, app_id, context, 0, urls, attributed)?.unwrap_or_else(|| {
        let mut root = backup;
        root.children.clear();
        root
    });
    Ok(root)
}
fn transform<E>(
    mut n: Node,
    app: &str,
    context: Context,
    depth: usize,
    urls: &mut crate::native::url::Shortener,
    attributed: &mut impl FnMut(
        &mut Node,
        &mut crate::native::url::Shortener,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<Option<Node>, E> {
    if depth > 100 {
        return Ok(Some(n));
    }
    filtered_actions(&mut n, context);
    // Exact single-target label association only. A target with a provider title
    // keeps that title; other missing descriptive metadata can still be supplied.
    let labels: Vec<_> = n
        .children
        .iter()
        .filter(|c| c.title_for.len() == 1 && !interactive(c))
        .map(|c| (c.identity.clone(), c.title_for[0].clone(), c.clone()))
        .collect();
    let mut associated = Vec::new();
    for (label_id, target_id, label) in labels {
        let count = n
            .children
            .iter()
            .filter(|c| c.identity == target_id)
            .count();
        if count == 1
            && let Some(target) = n.children.iter_mut().find(|c| c.identity == target_id)
        {
            if target.title.is_none() {
                target.title = display(&label).map(str::to_owned);
            }
            if target.help.is_none() {
                target.help = label.help;
            }
            if target.description.is_none() {
                target.description = label.description;
            }
            associated.push(label_id);
        }
    }
    n.children.retain(|c| !associated.contains(&c.identity));
    if n.selectable
        && !n.focused
        && n.role != "AXTable"
        && n.subrole.as_deref() != Some("AXTableRow")
        && n.children.iter().all(|c| {
            all(c, &|c| {
                !c.settable && !c.selectable && !c.focusable && !c.focused
            })
        })
    {
        let mut values = Vec::new();
        fn collect(n: &Node, values: &mut Vec<String>) {
            if let Some(s) = display(n) {
                values.push(s.into());
            }
            for c in &n.children {
                collect(c, values);
            }
        }
        for c in &n.children {
            collect(c, &mut values);
        }
        if n.description.is_none() && !values.is_empty() {
            n.description = Some(values.join(" "));
        }
        n.children.clear();
    }
    if !interactive(&n) && all(&n, &|c| !descriptive(c) && !interactive(c)) {
        return Ok(None);
    }
    if !n.enabled && !n.focused && n.children.is_empty() && !descriptive(&n) {
        return Ok(None);
    }
    if n.role == "AXGroup" && !n.selectable && !n.focused && !n.focusable && !descriptive(&n) {
        if n.children.is_empty() {
            return Ok(None);
        }
        if n.children.len() == 1 && n.children[0].available_ranges.is_none() {
            return transform(
                n.children.remove(0),
                app,
                context,
                depth + 1,
                urls,
                attributed,
            );
        }
    }
    if !n.selectable && !n.focused && !descriptive(&n) {
        let mut children = Vec::new();
        for mut c in n.children {
            if c.role == n.role
                && c.description == n.description
                && c.available_ranges.is_none()
                && !interactive(&c)
            {
                children.append(&mut c.children);
            } else {
                children.push(c);
            }
        }
        n.children = children;
    }
    if !n.focusable
        && !n.selectable
        && !n.focused
        && let Some(parent) = display(&n).map(str::to_owned)
    {
        fn splice(n: Node, parent: &str, out: &mut Vec<Node>) {
            let redundant = !interactive(&n)
                && n.role == "AXStaticText"
                && display(&n).is_some_and(|s| !s.trim().is_empty() && parent.contains(s.trim()))
                && n.children.iter().all(|c| all(c, &|c| !interactive(c)));
            if redundant {
                for c in n.children {
                    splice(c, parent, out);
                }
            } else {
                out.push(n);
            }
        }
        let mut children = Vec::new();
        for c in n.children {
            splice(c, &parent, &mut children);
        }
        n.children = children;
    }
    if n.role == "AXLink"
        && let Some(url) = &n.url
        && let Some(text) = urls.trim(url, false).filter(|s| !s.is_empty())
    {
        let mut mapped = MappedText::default();
        mapped.syntax(&text);
        n.semantic_value = Some(crate::native::render::SemanticText::url(
            url.clone(),
            false,
            text.clone(),
        ));
        n.role = "AXStaticText".into();
        n.role_description = Some("link".into());
        n.value = Some(text);
        n.mapped_value = Some(mapped);
        n.url = None;
        n.children.clear();
    }
    merge_text_only_siblings(&mut n, context, urls);
    if app == "com.apple.iCal"
        && n.role == "AXStaticText"
        && n.title.as_deref().or(n.description.as_deref()) == Some("Event")
        && n.children.len() == 1
        && n.children[0].role == "AXStaticText"
    {
        n.children.clear();
    }
    if n.role == "AXTextArea" {
        attributed(&mut n, urls)?;
    }
    let mut children = Vec::with_capacity(n.children.len());
    for child in n.children {
        if let Some(child) = transform(child, app, context, depth + 1, urls, attributed)? {
            children.push(child);
        }
    }
    n.children = children;
    Ok(Some(n))
}
