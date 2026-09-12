use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[path = "native/transform.rs"]
pub mod transform;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub identity: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub subrole: Option<String>,
    #[serde(default)]
    pub role_description: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub value_description: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(default)]
    pub numeric_value: bool,
    #[serde(default)]
    pub selectable: bool,
    #[serde(default)]
    pub focusable: bool,
    #[serde(default)]
    pub title_for: Vec<String>,
    #[serde(default)]
    pub available_ranges: Option<Vec<crate::selection::TextRange>>,
    #[serde(default)]
    pub mapped_value: Option<crate::selection::MappedText>,
    /// The local NSAttributedString source, before generated display syntax.
    /// Selection falls back to this value after rendered mapping fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_source: Option<String>,
    /// Global native range whose returned substring owns local source offsets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_range: Option<crate::selection::TextRange>,
    /// Retained URL interpolation metadata; literal Markdown is not a URL part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_value: Option<crate::native::render::SemanticText>,
    /// Immutable FullUI attribute rendering prepared before revision publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_attributes: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub window_id: Option<u32>,
    /// Transport for an independently captured focus view; extracted by Revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_tree: Option<Box<Node>>,
    #[serde(default)]
    pub settable: bool,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub frame: Option<[f64; 4]>,
    #[serde(default)]
    pub children: Vec<Node>,
    #[serde(default)]
    pub id: Option<u64>,
}
impl Default for Node {
    fn default() -> Self {
        Self {
            identity: Default::default(),
            role: Default::default(),
            subrole: Default::default(),
            role_description: Default::default(),
            title: Default::default(),
            description: Default::default(),
            value: Default::default(),
            value_description: Default::default(),
            placeholder: Default::default(),
            help: Default::default(),
            identifier: Default::default(),
            url: Default::default(),
            enabled: true,
            focused: Default::default(),
            selected: Default::default(),
            selected_text: Default::default(),
            numeric_value: false,
            selectable: Default::default(),
            focusable: Default::default(),
            title_for: Default::default(),
            available_ranges: Default::default(),
            mapped_value: Default::default(),
            attributed_source: Default::default(),
            truncation_range: Default::default(),
            semantic_value: Default::default(),
            prepared_attributes: Default::default(),
            detail: Default::default(),
            window_id: Default::default(),
            focus_tree: Default::default(),
            settable: Default::default(),
            actions: Default::default(),
            frame: Default::default(),
            children: Default::default(),
            id: Default::default(),
        }
    }
}

fn yes() -> bool {
    true
}

impl Node {
    pub fn semantic_eq(&self, other: &Node, ignore_value: bool) -> bool {
        self.role == other.role
            && self.subrole == other.subrole
            && self.role_description == other.role_description
            && self.title == other.title
            && self.description == other.description
            && (ignore_value || self.value == other.value)
            && (ignore_value || self.attributed_source == other.attributed_source)
            && (ignore_value || self.truncation_range == other.truncation_range)
            && self.value_description == other.value_description
            && self.placeholder == other.placeholder
            && self.help == other.help
            && self.identifier == other.identifier
            && self.url == other.url
    }
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for child in &self.children {
            child.walk(out);
        }
    }
    pub fn by_id(&self, id: u64) -> Option<&Node> {
        if self.id == Some(id) {
            Some(self)
        } else {
            self.children.iter().find_map(|n| n.by_id(id))
        }
    }
    pub fn by_identity(&self, id: &str) -> Option<&Node> {
        if self.identity == id {
            Some(self)
        } else {
            self.children.iter().find_map(|n| n.by_identity(id))
        }
    }
    pub fn by_identity_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.identity == id {
            Some(self)
        } else {
            self.children.iter_mut().find_map(|n| n.by_identity_mut(id))
        }
    }
    pub fn text(&self) -> String {
        let mut text = self
            .role_description
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| role_name(&self.role))
            .to_string();
        if matches!(self.role.as_str(), "AXMenuItem" | "AXMenuBarItem") {
            text.clear();
        }
        let mut flags = vec![];
        if !self.enabled {
            flags.push("disabled");
        }
        if self.selected {
            flags.push("selected");
        }
        if self.settable {
            flags.push("settable");
        }
        if self.numeric_value {
            flags.push("float");
        }
        if !flags.is_empty() {
            text.push_str(&format!(" ({})", flags.join(", ")));
        }
        if let Some(attributes) = &self.prepared_attributes {
            if !attributes.is_empty() {
                text.push(' ');
                text.push_str(attributes);
            }
            return text.trim_start().to_owned();
        }
        let title = self.title.as_deref().filter(|s| !s.is_empty());
        if let Some(title) = title {
            text.push(' ');
            text.push_str(title);
        }
        let has_value = self
            .mapped_value
            .as_ref()
            .map(|m| &m.text)
            .or(self.value.as_ref())
            .is_some_and(|v| !v.is_empty());
        if let Some(description) = self
            .description
            .as_deref()
            .filter(|d| !d.is_empty() && Some(*d) != title)
        {
            if title.is_none() && !has_value {
                text.push_str(&format!(" {description}"));
            } else {
                text.push_str(&format!(" Description: {description}"));
            }
        }
        if let Some(value) = self
            .mapped_value
            .as_ref()
            .map(|m| &m.text)
            .or(self.value.as_ref())
            && !value.is_empty()
        {
            if self.role == "AXStaticText"
                || (title.is_none()
                    && self.description.as_ref().is_none_or(|d| d.is_empty())
                    && matches!(self.role.as_str(), "AXScrollBar" | "AXValueIndicator"))
            {
                text.push(' ');
                text.push_str(value);
            } else {
                text.push_str(&format!(", Value: {value}"));
            }
        }
        if let Some(id) = &self.identifier {
            text.push_str(&format!(", ID: {id}"));
        }
        if let Some(help) = &self.help {
            text.push_str(&format!(" Help: {help}"));
        }
        if !self.actions.is_empty() {
            let separator = if title.is_some()
                || has_value
                || self.help.is_some()
                || self.description.is_some()
            {
                ", "
            } else {
                " "
            };
            text.push_str(&format!(
                "{separator}Secondary Actions: {}",
                self.actions
                    .iter()
                    .map(|a| action_label(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(placeholder) = &self.placeholder
            && !placeholder.is_empty()
        {
            text.push_str(&format!(", Placeholder: {placeholder}"));
        }
        if let Some(url) = &self.url {
            text.push_str(&format!(", URL: {url}"));
        }
        text.trim_start().to_owned()
    }
    /// Accept the observed display label or the underlying AX action name.
    /// Ambiguous localized/display labels never select an arbitrary action.
    pub fn action_named(&self, name: &str) -> Option<&str> {
        let mut matches = self
            .actions
            .iter()
            .filter(|action| action.as_str() == name || action_label(action) == name);
        let first = matches.next()?;
        matches.next().is_none().then_some(first.as_str())
    }
    fn rendered(&self, depth: usize, out: &mut Vec<(u64, usize, String)>) {
        out.push((self.id.expect("numbered revision"), depth, self.text()));
        for c in &self.children {
            c.rendered(
                if self.role == "AXWindow" && c.role == "AXMenuBar" {
                    depth
                } else {
                    depth + 1
                },
                out,
            );
        }
    }
}
pub fn role_name(role: &str) -> &str {
    match role {
        "AXWindow" => "standard window",
        "AXButton" => "button",
        "AXTextField" => "text field",
        "AXTextArea" => "text entry area",
        "AXStaticText" => "text",
        "AXScrollArea" => "scroll area",
        "AXScrollBar" => "scroll bar",
        "AXSlider" => "slider",
        "AXGroup" => "container",
        "AXMenu" => "menu",
        "AXMenuItem" | "AXMenuBarItem" => "",
        "AXMenuBar" => "menu bar",
        "AXCheckBox" => "check box",
        "AXPopUpButton" => "pop up button",
        "AXApplication" => "application",
        "AXRadioButton" => "radio button",
        "AXRadioGroup" => "radio group",
        "AXComboBox" => "combo box",
        "AXTable" => "table",
        "AXRow" => "row",
        "AXColumn" => "column",
        "AXCell" => "cell",
        "AXOutline" => "outline",
        "AXImage" => "image",
        "AXLink" => "link",
        "AXWebArea" => "web area",
        "AXToolbar" => "toolbar",
        "AXTabGroup" => "tab group",
        "AXSplitGroup" => "split group",
        "AXSplitter" => "splitter",
        "AXSheet" => "sheet",
        "AXDialog" => "dialog",
        "AXDrawer" => "drawer",
        "AXList" => "list",
        "AXProgressIndicator" => "progress indicator",
        "AXBusyIndicator" => "busy indicator",
        "AXDisclosureTriangle" => "disclosure triangle",
        "AXMenuButton" => "menu button",
        "AXValueIndicator" => "value indicator",
        "AXIncrementor" => "incrementor",
        "AXHeading" => "heading",
        _ => role,
    }
}

pub fn action_label(action: &str) -> &str {
    match action {
        "AXRaise" => "Raise",
        "AXCancel" => "Cancel",
        "AXScrollUpByPage" => "Scroll Up",
        "AXScrollDownByPage" => "Scroll Down",
        "AXScrollLeftByPage" => "Scroll Left",
        "AXScrollRightByPage" => "Scroll Right",
        "AXZoomWindow" => "zoom the window",
        _ => action,
    }
}

/// Public app-state grammar observed in both retained owned native fixture runs.
/// Unknown role/attribute specializations remain provider data, not inferred here.
pub fn format_state(title: &str, app_name: &str, description: &str, revision: &Revision) -> String {
    let full = description == revision.full_text();
    let mut state = if description.is_empty() {
        format!("There has been no change in the accessibility tree for Window: {title:?}.")
    } else if full {
        format!("Window: {title:?}, App: {app_name}.\n{description}\n")
    } else {
        format!(
            "The following is a diff from the previous accessibility tree for Window: {title:?} with ~ and + representing changed and added elements, respectively. Removed elements are summarized by ID range.\n{description}"
        )
    };
    let focus = revision.focus.as_ref().or_else(|| {
        let mut nodes = Vec::new();
        revision.root.walk(&mut nodes);
        nodes.into_iter().find(|node| node.focused)
    });
    if let Some(focus) = focus {
        if let Some(selected) = focus.selected_text.as_deref().filter(|s| !s.is_empty()) {
            state.push_str("\nSelected text: \x60\x60\x60\n");
            state.push_str(selected);
            state.push_str("\n\x60\x60\x60\n\nNote: Pay special attention to the content selected by the user. If the user asks a question or refers to the content they are looking at on-screen, they might be referring to the selected content (but they might be referring to something else that's visible, too).");
        } else if let Some(id) = focus.id {
            state.push_str(&format!(
                "\nThe focused UI element is {id} {}",
                focus.text()
            ));
        }
    }
    state
}

/// Earliest feasible longest strict increasing subsequence of old positions.
pub fn kept_positions(positions: &[usize]) -> BTreeSet<usize> {
    let mut lengths = vec![1; positions.len()];
    let mut tails: Vec<i128> = Vec::new();
    for i in (0..positions.len()).rev() {
        let value = -(positions[i] as i128);
        let slot = tails.partition_point(|v| *v < value);
        if slot == tails.len() {
            tails.push(value);
        } else {
            tails[slot] = value;
        }
        lengths[i] = slot + 1;
    }
    let mut remaining = tails.len();
    let mut previous = None;
    let mut keep = BTreeSet::new();
    for (i, &p) in positions.iter().enumerate() {
        if previous.is_none_or(|last| p > last) && lengths[i] >= remaining && remaining > 0 {
            keep.insert(i);
            previous = Some(p);
            remaining -= 1;
        }
    }
    keep
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub root: Node,
    #[serde(default)]
    pub focus: Option<Node>,
    pub generation: u64,
}
impl Revision {
    pub fn root(mut node: Node) -> Self {
        let mut focus = node.focus_tree.take().map(|n| *n);
        let mut next = 0;
        enumerate(&mut node, &mut next);
        let mut sources = HashMap::new();
        let mut nodes = Vec::new();
        node.walk(&mut nodes);
        for n in nodes {
            sources.insert(n.identity.clone(), n.id.unwrap());
        }
        if let Some(focus) = &mut focus {
            clear_ids(focus);
            // A finite capture cannot exhaust u64; root construction retains its
            // infallible API while append still checks externally supplied IDs.
            allocate(focus, &mut next, &mut sources).expect("bounded root allocation");
        }
        Self {
            root: node,
            focus,
            generation: 0,
        }
    }
    pub fn append(&self, mut root: Node) -> Result<Self> {
        let mut focus = root.focus_tree.take().map(|n| *n);
        clear_ids(&mut root);
        inherit(&self.root, &mut root);
        let mut nodes = vec![];
        root.walk(&mut nodes);
        let mut next = nodes
            .iter()
            .filter_map(|n| n.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::action("Element ID overflow"))?;
        let mut source_ids = HashMap::new();
        allocate(&mut root, &mut next, &mut source_ids)?;
        if let Some(focus) = &mut focus {
            clear_ids(focus);
            if let Some(old) = &self.focus {
                inherit(old, focus);
            }
            allocate(focus, &mut next, &mut source_ids)?;
        }
        Ok(Self {
            root,
            focus,
            generation: self
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::action("Revision generation overflow"))?,
        })
    }
    pub fn by_id(&self, id: u64) -> Option<&Node> {
        self.root
            .by_id(id)
            .or_else(|| self.focus.as_ref().and_then(|f| f.by_id(id)))
    }
    pub fn focus_text(&self) -> Option<String> {
        self.focus.as_ref().map(|focus| {
            let mut rows = Vec::new();
            focus.rendered(0, &mut rows);
            rows.into_iter()
                .map(|(id, d, text)| format!("{}{id} {text}", "\t".repeat(d)))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }
    fn unique_nodes(&self) -> Vec<&Node> {
        let mut all = Vec::new();
        self.root.walk(&mut all);
        if let Some(focus) = &self.focus {
            focus.walk(&mut all);
        }
        let mut seen = HashSet::new();
        all.retain(|n| seen.insert(n.identity.as_str()));
        all
    }
    pub fn full_text(&self) -> String {
        let mut rows = vec![];
        self.root.rendered(0, &mut rows);
        rows.into_iter()
            .map(|(id, d, s)| {
                let detail = self
                    .root
                    .by_id(id)
                    .and_then(|n| n.detail.as_deref())
                    .filter(|d| !d.is_empty())
                    .map(|d| format!(" {d}"))
                    .unwrap_or_default();
                format!("{}{id} {s}{detail}", "\t".repeat(d))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn diff(&self, old: &Self) -> Option<String> {
        self.diff_with_options(old, DiffOptions::default())
    }
    pub fn diff_with_options(&self, old: &Self, options: DiffOptions) -> Option<String> {
        let mut changes = Vec::new();
        differences(&old.root, &self.root, vec![], &mut changes);
        changes.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.kind.order().cmp(&b.kind.order()))
        });
        let mut nodes = Vec::new();
        self.root.walk(&mut nodes);
        let mut remaining = nodes.len();
        let mut removed = BTreeSet::new();
        let mut had_removal = false;
        let mut lines = Vec::new();
        for change in changes {
            if change.kind == Kind::Remove && options.summarize_removed {
                had_removal = true;
                let mut nodes = Vec::new();
                change.node.walk(&mut nodes);
                removed.extend(nodes.into_iter().filter_map(|n| n.id));
                continue;
            }
            let expanded = match change.kind {
                Kind::Insert => options.expand_inserts,
                Kind::Remove => options.expand_removals,
                Kind::Update => false,
            };
            let mut rows = Vec::new();
            render_change(change.node, change.path.len(), expanded, &mut rows);
            if !options.ignore_line_budget {
                remaining = remaining.checked_sub(rows.len())?;
            }
            for (node, depth) in rows {
                let mut line = format!("{}{}", change.kind.marker(), "\t".repeat(depth));
                if options.include_ids
                    && let Some(id) = node.id
                {
                    line.push_str(&format!("{id} "));
                }
                line.push_str(&node.text());
                if options.include_details
                    && let Some(detail) = &node.detail
                    && !detail.is_empty()
                {
                    line.push(' ');
                    line.push_str(detail);
                }
                lines.push(line);
            }
        }
        if had_removal && remaining == 0 {
            return None;
        }
        if !removed.is_empty() {
            lines.insert(0, format!("Removed element IDs: {}", ranges(&removed)));
        }
        Some(lines.join("\n"))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct DiffOptions {
    pub summarize_removed: bool,
    pub include_ids: bool,
    pub include_details: bool,
    pub expand_inserts: bool,
    pub expand_removals: bool,
    pub ignore_line_budget: bool,
}
impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            summarize_removed: true,
            include_ids: true,
            include_details: true,
            expand_inserts: true,
            expand_removals: true,
            ignore_line_budget: false,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Remove,
    Insert,
    Update,
}
impl Kind {
    fn order(self) -> u8 {
        match self {
            Self::Remove => 1,
            Self::Insert => 2,
            Self::Update => 3,
        }
    }
    fn marker(self) -> char {
        match self {
            Self::Remove => '-',
            Self::Insert => '+',
            Self::Update => '~',
        }
    }
}
struct Change<'a> {
    node: &'a Node,
    path: Vec<usize>,
    kind: Kind,
}
fn differences<'a>(old: &'a Node, new: &'a Node, path: Vec<usize>, out: &mut Vec<Change<'a>>) {
    if old.id != new.id || old.identity != new.identity {
        out.push(Change {
            node: old,
            path: path.clone(),
            kind: Kind::Remove,
        });
        out.push(Change {
            node: new,
            path,
            kind: Kind::Insert,
        });
        return;
    }
    if old.text() != new.text() {
        out.push(Change {
            node: new,
            path: path.clone(),
            kind: Kind::Update,
        });
    }
    let old_map: HashMap<_, _> = old
        .children
        .iter()
        .enumerate()
        .map(|(i, n)| ((n.id, n.identity.as_str()), i))
        .collect();
    let mut matched = HashSet::new();
    for (i, child) in new.children.iter().enumerate() {
        let mut child_path = path.clone();
        child_path.push(i);
        if let Some(&old_i) = old_map.get(&(child.id, child.identity.as_str())) {
            matched.insert(old_i);
            differences(&old.children[old_i], child, child_path, out);
        } else {
            out.push(Change {
                node: child,
                path: child_path,
                kind: Kind::Insert,
            });
        }
    }
    for (i, child) in old.children.iter().enumerate() {
        if !matched.contains(&i) {
            let mut child_path = path.clone();
            child_path.push(i);
            out.push(Change {
                node: child,
                path: child_path,
                kind: Kind::Remove,
            });
        }
    }
}
fn render_change<'a>(
    node: &'a Node,
    depth: usize,
    expanded: bool,
    out: &mut Vec<(&'a Node, usize)>,
) {
    out.push((node, depth));
    if expanded {
        for child in &node.children {
            render_change(child, depth + 1, true, out);
        }
    }
}

fn enumerate(n: &mut Node, next: &mut u64) {
    n.id = Some(*next);
    *next += 1;
    for c in &mut n.children {
        enumerate(c, next);
    }
}
fn clear_ids(n: &mut Node) {
    n.id = None;
    for c in &mut n.children {
        clear_ids(c);
    }
}
fn inherit(old: &Node, new: &mut Node) {
    if old.identity != new.identity {
        return;
    }
    new.id = old.id;
    let mut start = 0;
    while start < old.children.len().min(new.children.len())
        && old.children[start].identity == new.children[start].identity
    {
        inherit(&old.children[start], &mut new.children[start]);
        start += 1;
    }
    let old_indices: HashMap<_, _> = old
        .children
        .iter()
        .enumerate()
        .skip(start)
        .map(|(i, n)| (n.identity.as_str(), i))
        .collect();
    let pairs: Vec<_> = new
        .children
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(|(i, n)| old_indices.get(n.identity.as_str()).map(|p| (i, *p)))
        .collect();
    for i in kept_positions(&pairs.iter().map(|p| p.1).collect::<Vec<_>>()) {
        let (new_i, old_i) = pairs[i];
        inherit(&old.children[old_i], &mut new.children[new_i]);
    }
}
fn allocate(n: &mut Node, next: &mut u64, sources: &mut HashMap<String, u64>) -> Result<()> {
    let id = match n.id.or_else(|| sources.get(&n.identity).copied()) {
        Some(id) => id,
        None => {
            let id = *next;
            *next = next
                .checked_add(1)
                .ok_or_else(|| Error::action("Element ID overflow"))?;
            id
        }
    };
    n.id = Some(id);
    sources.insert(n.identity.clone(), id);
    for c in &mut n.children {
        allocate(c, next, sources)?;
    }
    Ok(())
}
pub fn ranges(ids: &BTreeSet<u64>) -> String {
    let mut result = vec![];
    let mut iter = ids.iter().copied().peekable();
    while let Some(first) = iter.next() {
        let mut last = first;
        while iter.peek().is_some_and(|x| last.checked_add(1) == Some(*x)) {
            last = iter.next().unwrap();
        }
        result.push(if first == last {
            first.to_string()
        } else {
            format!("{first}-{last}")
        });
    }
    result.join(", ")
}

#[derive(Default)]
pub struct Sessions {
    pub revisions: BTreeMap<String, Revision>,
}
impl Sessions {
    pub fn observe(&mut self, app: &str, root: Node, full: bool) -> Result<(String, Revision)> {
        let old = self.revisions.get(app).filter(|_| !full);
        let current = match old {
            Some(old) => old.append(root)?,
            None => Revision::root(root),
        };
        let text = match old {
            Some(old) => current.diff(old).unwrap_or_else(|| current.full_text()),
            None => current.full_text(),
        };
        self.revisions.insert(app.into(), current.clone());
        Ok((text, current))
    }
    /// Always collect fresh data in this implementation. Publish before post-match
    /// checks, matching the recovered failure ordering; no silent monitor failure.
    pub fn resolve(&mut self, app: &str, id: u64, fresh: Node, ignore_value: bool) -> Result<Node> {
        let old = self
            .revisions
            .get(app)
            .ok_or_else(|| Error::action("Get app state before acting"))?
            .clone();
        let target = old
            .by_id(id)
            .ok_or_else(|| Error::action(format!("Invalid element ID: {id}")))?
            .clone();
        if let Some(current) = fresh.by_identity(&target.identity).or_else(|| {
            fresh
                .focus_tree
                .as_ref()
                .and_then(|f| f.by_identity(&target.identity))
        }) && target.semantic_eq(current, ignore_value)
        {
            return Ok(current.clone());
        }
        let nodes = old.unique_nodes();
        if nodes
            .iter()
            .filter(|n| target.semantic_eq(n, false))
            .count()
            != 1
        {
            return Err(Error::action("Element ambiguous before refetch"));
        }
        let revision = old.append(fresh)?;
        self.revisions.insert(app.into(), revision.clone());
        let matches: Vec<_> = revision
            .unique_nodes()
            .into_iter()
            .filter(|n| target.semantic_eq(n, ignore_value))
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Error::action("Element no longer valid after refetch")),
            _ => Err(Error::action("Element ambiguous after refetch")),
        }
    }
}
