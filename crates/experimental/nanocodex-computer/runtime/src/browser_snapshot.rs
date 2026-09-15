//! CSS matching over immutable captured DOM data. No browser or host authority.
use cssparser::{CowRcStr, ParseError, ParserInput, SourceLocation, ToCss};
use html5ever::Namespace;
use scraper::selector::{CssLocalName, CssString};
use selectors::{
    Element as SelectorElement, OpaqueElement,
    attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint},
    bloom::BloomFilter,
    matching::{self, ElementSelectorFlags, MatchingContext},
    parser::{self, ParseRelative, SelectorList, SelectorParseErrorKind},
};
use serde_json::{Value, json};
use std::{cell::Cell, collections::HashMap, fmt, time::Instant};

const HTML: &str = "http://www.w3.org/1999/xhtml";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const MAX_NODES: usize = 20_000;
const MAX_DEPTH: usize = 512;
const MAX_SELECTOR_BYTES: usize = 16_384;
const MAX_STEPS: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
struct DomSelector;
impl parser::SelectorImpl for DomSelector {
    type AttrValue = CssString;
    type Identifier = CssLocalName;
    type LocalName = CssLocalName;
    type NamespacePrefix = CssLocalName;
    type NamespaceUrl = Namespace;
    type BorrowedNamespaceUrl = Namespace;
    type BorrowedLocalName = CssLocalName;
    type NonTSPseudoClass = State;
    type PseudoElement = Generated;
    type ExtraMatchingData<'a> = ();
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct State {
    name: String,
    arguments: Vec<String>,
}
impl ToCss for State {
    fn to_css<W: fmt::Write>(&self, out: &mut W) -> fmt::Result {
        write!(out, ":{}", self.name)?;
        if !self.arguments.is_empty() {
            out.write_char('(')?;
            for (i, arg) in self.arguments.iter().enumerate() {
                if i != 0 {
                    out.write_char(',')?;
                }
                cssparser::serialize_string(arg, out)?;
            }
            out.write_char(')')?;
        }
        Ok(())
    }
}
impl parser::NonTSPseudoClass for State {
    type Impl = DomSelector;
    fn is_active_or_hover(&self) -> bool {
        matches!(self.name.as_str(), "active" | "hover")
    }
    fn is_user_action_state(&self) -> bool {
        matches!(
            self.name.as_str(),
            "active" | "hover" | "focus" | "focus-visible" | "focus-within"
        )
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Generated(String);
impl ToCss for Generated {
    fn to_css<W: fmt::Write>(&self, out: &mut W) -> fmt::Result {
        write!(out, "::{}", self.0)
    }
}
impl parser::PseudoElement for Generated {
    type Impl = DomSelector;
    fn is_before_or_after(&self) -> bool {
        matches!(self.0.as_str(), "before" | "after")
    }
}
const STATES: &[&str] = &[
    "active",
    "hover",
    "focus",
    "focus-visible",
    "focus-within",
    "enabled",
    "disabled",
    "checked",
    "indeterminate",
    "default",
    "required",
    "optional",
    "valid",
    "invalid",
    "in-range",
    "out-of-range",
    "read-only",
    "read-write",
    "placeholder-shown",
    "autofill",
    "-webkit-autofill",
    "defined",
    "target",
    "target-within",
    "any-link",
    "link",
    "visited",
    "fullscreen",
    "modal",
    "popover-open",
    "open",
    "user-valid",
    "user-invalid",
    "picture-in-picture",
    "playing",
    "paused",
    "seeking",
    "buffering",
    "stalled",
    "muted",
    "volume-locked",
];
struct DomParser<'a> {
    supported: Option<&'a Vec<String>>,
    lang_ranges: bool,
}
impl<'i> parser::Parser<'i> for DomParser<'_> {
    type Impl = DomSelector;
    type Error = SelectorParseErrorKind<'i>;
    fn parse_is_and_where(&self) -> bool {
        true
    }
    fn parse_has(&self) -> bool {
        true
    }
    fn parse_nth_child_of(&self) -> bool {
        true
    }
    fn parse_host(&self) -> bool {
        true
    }
    fn parse_part(&self) -> bool {
        true
    }
    fn parse_slotted(&self) -> bool {
        true
    }
    fn parse_non_ts_pseudo_class(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<State, ParseError<'i, Self::Error>> {
        let lower = name.to_ascii_lowercase();
        if STATES.contains(&lower.as_str())
            && self.supported.is_none_or(|states| states.contains(&lower))
        {
            return Ok(State {
                name: lower,
                arguments: vec![],
            });
        }
        Err(
            location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                name,
            )),
        )
    }
    fn parse_non_ts_functional_pseudo_class<'t>(
        &self,
        name: CowRcStr<'i>,
        input: &mut cssparser::Parser<'i, 't>,
        _: bool,
    ) -> Result<State, ParseError<'i, Self::Error>> {
        let lower = name.to_ascii_lowercase();
        let arguments = match lower.as_str() {
            "lang" if self.lang_ranges => {
                input.parse_comma_separated(|p| Ok(p.expect_ident_or_string()?.to_string()))?
            }
            "lang" => vec![input.expect_ident()?.to_string()],
            "dir" => {
                let value = input.expect_ident()?.to_ascii_lowercase();
                if !matches!(value.as_str(), "ltr" | "rtl") {
                    return Err(input.new_custom_error(
                        SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
                    ));
                }
                vec![value]
            }
            _ => {
                return Err(input.new_custom_error(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
                ));
            }
        };
        Ok(State {
            name: lower,
            arguments,
        })
    }
    fn parse_pseudo_element(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<Generated, ParseError<'i, Self::Error>> {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "before"
                | "after"
                | "first-line"
                | "first-letter"
                | "marker"
                | "selection"
                | "backdrop"
                | "placeholder"
                | "file-selector-button"
                | "details-content"
                | "target-text"
                | "spelling-error"
                | "grammar-error"
        ) || lower.starts_with("-webkit-")
        {
            return Ok(Generated(lower));
        }
        Err(
            location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                name,
            )),
        )
    }
}
#[derive(Debug)]
struct Attribute {
    name: CssLocalName,
    namespace: Namespace,
    value: String,
}
#[derive(Debug)]
struct Node {
    id: String,
    tag: CssLocalName,
    namespace: Namespace,
    attributes: Vec<Attribute>,
    parent: Option<usize>,
    previous: Option<usize>,
    next: Option<usize>,
    children: Vec<usize>,
    tree: String,
    host: Option<usize>,
    text: bool,
    states: Vec<String>,
    language: Option<String>,
}
#[derive(Debug)]
pub struct Snapshot {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    roots: HashMap<String, Vec<usize>>,
    html: bool,
    quirks: bool,
    supported: Option<Vec<String>>,
    lang_ranges: bool,
}
#[derive(Debug)]
struct Budget {
    steps: Cell<usize>,
    exceeded: Cell<bool>,
    deadline: Instant,
}
impl Budget {
    fn step(&self) -> bool {
        let steps = self.steps.get().saturating_add(1);
        self.steps.set(steps);
        if steps > MAX_STEPS || Instant::now() >= self.deadline {
            self.exceeded.set(true);
        }
        !self.exceeded.get()
    }
}
#[derive(Clone, Copy, Debug)]
struct Element<'a> {
    snapshot: &'a Snapshot,
    position: usize,
    budget: &'a Budget,
}
impl<'a> Element<'a> {
    fn node(&self) -> &'a Node {
        &self.snapshot.nodes[self.position]
    }
    fn at(&self, position: Option<usize>) -> Option<Self> {
        self.budget
            .step()
            .then_some(position)
            .flatten()
            .map(|position| Self { position, ..*self })
    }
    fn attr(&self, name: &str) -> Option<&'a str> {
        self.node()
            .attributes
            .iter()
            .find(|a| a.namespace.is_empty() && a.name.0.as_ref() == name)
            .map(|a| a.value.as_str())
    }
    fn language(&self) -> Option<&'a str> {
        let mut current = Some(*self);
        while let Some(node) = current {
            if !self.budget.step() {
                return None;
            }
            if let Some(lang) = node.node().language.as_deref() {
                return Some(lang);
            }
            if let Some(attr) = node
                .node()
                .attributes
                .iter()
                .find(|a| a.namespace.as_ref() == XML && a.name.0.as_ref() == "lang")
            {
                return Some(&attr.value);
            }
            if let Some(lang) = node.attr("lang") {
                return Some(lang);
            }
            current = node
                .parent_element()
                .or_else(|| node.containing_shadow_host());
        }
        None
    }
}
impl SelectorElement for Element<'_> {
    type Impl = DomSelector;
    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.node())
    }
    fn parent_element(&self) -> Option<Self> {
        self.at(self.node().parent)
    }
    fn parent_node_is_shadow_root(&self) -> bool {
        self.budget.step() && self.node().parent.is_none() && self.node().host.is_some()
    }
    fn containing_shadow_host(&self) -> Option<Self> {
        self.at(self.node().host)
    }
    fn is_pseudo_element(&self) -> bool {
        false
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        self.at(self.node().previous)
    }
    fn next_sibling_element(&self) -> Option<Self> {
        self.at(self.node().next)
    }
    fn first_element_child(&self) -> Option<Self> {
        self.at(self.node().children.first().copied())
    }
    fn is_html_element_in_html_document(&self) -> bool {
        self.snapshot.html && self.node().namespace.as_ref() == HTML
    }
    fn has_local_name(&self, name: &CssLocalName) -> bool {
        self.budget.step()
            && (self.node().tag == *name
                || self.snapshot.html && self.node().tag.0.eq_ignore_ascii_case(&name.0))
    }
    fn has_namespace(&self, ns: &Namespace) -> bool {
        self.budget.step() && self.node().namespace == *ns
    }
    fn is_same_type(&self, other: &Self) -> bool {
        self.budget.step()
            && self.node().tag == other.node().tag
            && self.node().namespace == other.node().namespace
    }
    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        name: &CssLocalName,
        op: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        self.budget.step()
            && self.node().attributes.iter().any(|a| {
                !matches!(*ns, NamespaceConstraint::Specific(url) if *url != a.namespace)
                    && (a.name == *name
                        || self.snapshot.html && a.name.0.eq_ignore_ascii_case(&name.0))
                    && op.eval_str(&a.value)
            })
    }
    fn match_non_ts_pseudo_class(
        &self,
        state: &State,
        _: &mut MatchingContext<'_, DomSelector>,
    ) -> bool {
        if !self.budget.step() {
            return false;
        }
        match state.name.as_str() {
            "visited" => false,
            "lang" => self.language().is_some_and(|language| {
                state
                    .arguments
                    .iter()
                    .any(|range| language_matches(language, range))
            }),
            "dir" => self
                .node()
                .states
                .iter()
                .any(|s| s == &format!("dir({})", state.arguments[0])),
            name => self.node().states.iter().any(|s| s == name),
        }
    }
    fn match_pseudo_element(
        &self,
        _: &Generated,
        _: &mut MatchingContext<'_, DomSelector>,
    ) -> bool {
        false
    }
    fn apply_selector_flags(&self, _: ElementSelectorFlags) {}
    fn is_link(&self) -> bool {
        self.budget.step() && self.node().states.iter().any(|s| s == "any-link")
    }
    fn is_html_slot_element(&self) -> bool {
        self.node().namespace.as_ref() == HTML && self.node().tag.0.as_ref() == "slot"
    }
    fn has_id(&self, id: &CssLocalName, sensitivity: CaseSensitivity) -> bool {
        self.budget.step()
            && self
                .attr("id")
                .is_some_and(|v| sensitivity.eq(v.as_bytes(), id.0.as_bytes()))
    }
    fn has_class(&self, name: &CssLocalName, sensitivity: CaseSensitivity) -> bool {
        self.budget.step()
            && self.attr("class").is_some_and(|v| {
                v.split([' ', '\t', '\r', '\n', '\x0c'])
                    .any(|v| !v.is_empty() && sensitivity.eq(v.as_bytes(), name.0.as_bytes()))
            })
    }
    fn has_custom_state(&self, _: &CssLocalName) -> bool {
        false
    }
    fn imported_part(&self, _: &CssLocalName) -> Option<CssLocalName> {
        None
    }
    fn is_part(&self, _: &CssLocalName) -> bool {
        false
    }
    fn is_empty(&self) -> bool {
        self.budget.step() && self.node().children.is_empty() && !self.node().text
    }
    fn is_root(&self) -> bool {
        self.budget.step() && self.node().parent.is_none() && self.node().tree.is_empty()
    }
    fn add_element_unique_hashes(&self, _: &mut BloomFilter) -> bool {
        false
    }
}
fn language_matches(language: &str, range: &str) -> bool {
    if language.is_empty() {
        return false;
    }
    let language = language.to_ascii_lowercase();
    let range = range.to_ascii_lowercase();
    let mut tags = language.split('-');
    let mut wanted = range.split('-');
    let first = wanted.next().unwrap_or("");
    let first_tag = tags.next();
    if first != "*" && first_tag != Some(first) {
        return false;
    }
    for part in wanted {
        if part == "*" {
            continue;
        }
        loop {
            match tags.next() {
                Some(tag) if tag == part => break,
                Some(tag) if tag.len() > 1 => continue,
                _ => return false,
            }
        }
    }
    true
}
impl Snapshot {
    pub fn new(value: &Value) -> Result<Self, String> {
        let source = value["nodes"]
            .as_array()
            .ok_or("Snapshot nodes must be an array")?;
        if source.len() > MAX_NODES {
            return Err("Snapshot node limit exceeded".into());
        }
        let mut nodes = Vec::with_capacity(source.len());
        let mut index = HashMap::new();
        for (i, v) in source.iter().enumerate() {
            let id = v["id"]
                .as_str()
                .ok_or("Snapshot node has no identity")?
                .to_owned();
            if index.insert(id.clone(), i).is_some() {
                return Err("Duplicate snapshot node identity".into());
            }
            let metadata: HashMap<&str, &Value> = v["attributeNamespaces"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["name"].as_str().map(|name| (name, a)))
                .collect();
            if v["attributes"]
                .as_object()
                .is_some_and(|attrs| attrs.len() > 4096)
            {
                return Err("Snapshot attribute limit exceeded".into());
            }
            let attributes = if let Some(records) = v["attributeRecords"].as_array() {
                if records.len() > 4096 {
                    return Err("Snapshot attribute limit exceeded".into());
                }
                records
                    .iter()
                    .map(|a| Attribute {
                        name: CssLocalName::from(
                            a["localName"]
                                .as_str()
                                .or_else(|| a["name"].as_str())
                                .unwrap_or(""),
                        ),
                        namespace: Namespace::from(a["namespaceURI"].as_str().unwrap_or("")),
                        value: a["value"].as_str().unwrap_or("").into(),
                    })
                    .collect()
            } else {
                v["attributes"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, value)| {
                        let m = metadata.get(name.as_str()).copied();
                        Attribute {
                            name: CssLocalName::from(
                                m.and_then(|m| m["localName"].as_str()).unwrap_or(name),
                            ),
                            namespace: Namespace::from(
                                m.and_then(|m| m["namespaceURI"].as_str()).unwrap_or(""),
                            ),
                            value: value.as_str().unwrap_or("").into(),
                        }
                    })
                    .collect()
            };
            nodes.push(Node {
                id,
                tag: CssLocalName::from(v["tag"].as_str().unwrap_or("")),
                namespace: Namespace::from(
                    v.get("namespaceURI")
                        .map(|value| value.as_str().unwrap_or(""))
                        .unwrap_or(HTML),
                ),
                attributes,
                parent: None,
                previous: None,
                next: None,
                children: vec![],
                tree: v["tree"].as_str().unwrap_or("").into(),
                host: None,
                text: v["hasDirectText"]
                    .as_bool()
                    .unwrap_or_else(|| v["textContent"].as_str().is_some_and(|s| !s.is_empty())),
                states: v["cssStates"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                language: v["language"].as_str().map(str::to_owned),
            });
        }
        let mut roots: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, v) in source.iter().enumerate() {
            if let Some(parent_id) = v["parent"].as_str() {
                let parent = *index.get(parent_id).ok_or("Snapshot parent is absent")?;
                if parent == i || nodes[parent].tree != nodes[i].tree {
                    return Err("Invalid snapshot parent".into());
                }
                nodes[i].parent = Some(parent);
                if let Some(previous) = nodes[parent].children.last().copied() {
                    nodes[i].previous = Some(previous);
                    nodes[previous].next = Some(i);
                }
                nodes[parent].children.push(i);
            } else {
                let siblings = roots.entry(nodes[i].tree.clone()).or_default();
                if let Some(previous) = siblings.last().copied() {
                    nodes[i].previous = Some(previous);
                    nodes[previous].next = Some(i);
                }
                siblings.push(i);
            }
            nodes[i].host = v["shadowHost"]
                .as_str()
                .map(|id| {
                    index
                        .get(id)
                        .copied()
                        .ok_or("Snapshot shadow host is absent")
                })
                .transpose()?;
        }
        // Bound malformed input cycles and traversal depth before any recursive selector matching.
        for i in 0..nodes.len() {
            let mut current = Some(i);
            let mut depth = 0;
            while let Some(position) = current {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err("Snapshot ancestry is cyclic or exceeds depth limit".into());
                }
                current = nodes[position].parent.or(nodes[position].host);
            }
        }
        Ok(Self {
            nodes,
            index,
            roots,
            lang_ranges: value["selectorFeatures"]["langRanges"]
                .as_bool()
                .unwrap_or(false),
            supported: value["supportedStates"].as_array().map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
            html: value["contentType"].as_str().unwrap_or("text/html") == "text/html",
            quirks: value["compatMode"].as_str() == Some("BackCompat"),
        })
    }
    pub fn query(&self, css: &str, root: &str, operation: &str, deadline: Instant) -> Value {
        match self.select(css, root, operation, deadline) {
            Ok(ids) => json!({"ids":ids}),
            Err(error) => json!({"error":error}),
        }
    }
    fn select(
        &self,
        css: &str,
        root: &str,
        operation: &str,
        deadline: Instant,
    ) -> Result<Vec<&str>, &'static str> {
        if css.len() > MAX_SELECTOR_BYTES
            || css.bytes().filter(|b| *b == b'(' || *b == b'[').count() > 128
        {
            return Err("budget");
        }
        let mut input = ParserInput::new(css);
        let selectors = SelectorList::parse(
            &DomParser {
                supported: self.supported.as_ref(),
                lang_ranges: self.lang_ranges,
            },
            &mut cssparser::Parser::new(&mut input),
            ParseRelative::No,
        )
        .map_err(|_| "syntax")?;
        let budget = Budget {
            steps: Cell::new(0),
            exceeded: Cell::new(false),
            deadline,
        };
        let element = self.index.get(root).copied();
        let scope = element.or_else(|| {
            if root.is_empty() {
                self.roots.get("").and_then(|v| v.first().copied())
            } else {
                None
            }
        });
        let mut candidates = Vec::new();
        match operation {
            "matches" => candidates.extend(element),
            "closest" => {
                let mut current = element;
                while let Some(i) = current {
                    candidates.push(i);
                    current = self.nodes[i].parent;
                }
            }
            "query" | "first" => {
                let children = element
                    .map(|i| &self.nodes[i].children)
                    .or_else(|| self.roots.get(root));
                let mut stack = children
                    .into_iter()
                    .flatten()
                    .rev()
                    .copied()
                    .collect::<Vec<_>>();
                while let Some(i) = stack.pop() {
                    candidates.push(i);
                    stack.extend(self.nodes[i].children.iter().rev().copied());
                }
            }
            _ => return Err("operation"),
        }
        let mut caches = matching::SelectorCaches::default();
        let mut context = MatchingContext::new(
            matching::MatchingMode::Normal,
            None,
            &mut caches,
            if self.quirks {
                matching::QuirksMode::Quirks
            } else {
                matching::QuirksMode::NoQuirks
            },
            matching::NeedsSelectorFlags::No,
            matching::MatchingForInvalidation::No,
        );
        // A ShadowRoot is not an Element; it must not acquire :scope or :root.
        context.scope_element = scope.map(|i| OpaqueElement::new(&self.nodes[i]));
        context.current_host = element
            .and_then(|i| self.nodes[i].host)
            .or_else(|| {
                self.roots
                    .get(root)
                    .and_then(|v| v.first())
                    .and_then(|i| self.nodes[*i].host)
            })
            .map(|i| OpaqueElement::new(&self.nodes[i]));
        let mut ids = Vec::new();
        for position in candidates {
            if !budget.step() {
                break;
            }
            let element = Element {
                snapshot: self,
                position,
                budget: &budget,
            };
            if selectors
                .slice()
                .iter()
                .any(|s| matching::matches_selector(s, 0, None, &element, &mut context))
            {
                ids.push(self.nodes[position].id.as_str());
                if operation != "query" {
                    break;
                }
            }
        }
        if budget.exceeded.get() || Instant::now() >= deadline {
            return Err("budget");
        }
        Ok(ids)
    }
    /// Installs only a data query closure. The adapter immediately hides the global binding.
    pub fn install(self, ctx: &rquickjs::Ctx<'_>, deadline: Instant) -> rquickjs::Result<()> {
        ctx.globals().set(
            "__skyre_snapshot_query",
            rquickjs::function::Func::from(move |css: String, root: String, operation: String| {
                self.query(&css, &root, &operation, deadline).to_string()
            }),
        )
    }
}
