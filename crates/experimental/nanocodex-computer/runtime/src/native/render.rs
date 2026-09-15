//! Observation-owned native rendering. No URL parsing opens a resource.
//! Semantic URL parts are retained independently from literal Markdown.
use super::url::Shortener;
use crate::{Error, Result, ax::Node, selection::MappedText};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

const MAX_TEXT_UNITS: usize = 4 * 1024 * 1024;
const MAX_PARTS: usize = 65536;

/// Native references have already been validated against the approved PID.
/// Shortener effects are deferred until the owning retained node is rendered.
#[derive(Clone, Debug)]
pub struct DeferredRun {
    pub run: crate::rich_text::AttributedRun,
    pub link: Option<(String, bool)>,
    pub attachment_url: Option<String>,
}
#[derive(Clone, Debug)]
pub struct AttributedInput {
    pub source: String,
    pub runs: Vec<DeferredRun>,
}
impl DeferredRun {
    pub fn retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>();
        for text in [
            self.link.as_ref().map(|v| v.0.as_str()),
            self.attachment_url.as_deref(),
            self.run.style.link.as_deref(),
            self.run
                .style
                .attachment
                .as_ref()
                .and_then(|v| v.description.as_deref()),
            self.run
                .style
                .attachment
                .as_ref()
                .and_then(|v| v.role_description.as_deref()),
            self.run
                .style
                .attachment
                .as_ref()
                .and_then(|v| v.image_url.as_deref()),
            self.run.style.list.as_ref().map(|v| v.marker.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            bytes = bytes.saturating_add(text.len());
        }
        bytes
    }
}
impl AttributedInput {
    pub fn retained_bytes(&self) -> usize {
        self.runs.iter().fold(self.source.len(), |bytes, run| {
            bytes.saturating_add(run.retained_bytes())
        })
    }
    pub fn render(&self, urls: &mut Shortener) -> Result<MappedText> {
        if self.retained_bytes() > 16 * 1024 * 1024 || self.runs.len() > MAX_PARTS {
            return Err(Error::action(
                "Attributed preparation exceeds retained-input bound",
            ));
        }
        let mut runs = Vec::with_capacity(self.runs.len());
        for deferred in &self.runs {
            let mut run = deferred.run.clone();
            if let Some((url, image)) = &deferred.link {
                run.style.link = urls.trim(url, *image);
            }
            if let Some(url) = &deferred.attachment_url
                && let Some(attachment) = &mut run.style.attachment
            {
                attachment.image_url = urls.trim(url, true);
            }
            check_urls(urls)?;
            runs.push(run);
        }
        crate::rich_text::render_attributed(&self.source, &runs)
    }
    /// Preserve the exact local source used by original sourceTextRange's
    /// attributedString.string fallback (10072e128..10072e160).
    pub fn render_into(&self, node: &mut Node, urls: &mut Shortener) -> Result<()> {
        let mapped = self.render(urls)?;
        node.attributed_source = Some(self.source.clone());
        node.value = Some(mapped.text.clone());
        node.mapped_value = Some(mapped);
        // Attributed destinations are literals, not generic URL interpolation.
        node.semantic_value = None;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Part {
    Literal {
        mapped: MappedText,
    },
    Url {
        original: String,
        is_image: bool,
        display: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SemanticText {
    pub parts: Vec<Part>,
}
impl SemanticText {
    pub fn url(original: String, is_image: bool, display: String) -> Self {
        Self {
            parts: vec![Part::Url {
                original,
                is_image,
                display,
            }],
        }
    }
    pub fn mapped(&self) -> Result<MappedText> {
        if self.parts.len() > MAX_PARTS {
            return Err(Error::action("Semantic text exceeds part bound"));
        }
        let mut out = MappedText::default();
        for part in &self.parts {
            let text = match part {
                Part::Literal { mapped } => {
                    if mapped.text.encode_utf16().count() != mapped.source_offsets.len() {
                        return Err(Error::action("Semantic text has an invalid source map"));
                    }
                    &mapped.text
                }
                Part::Url {
                    original, display, ..
                } => {
                    if original.len() > 1024 * 1024 {
                        return Err(Error::action("Semantic URL exceeds length bound"));
                    }
                    display
                }
            };
            if text.len() > 4 * MAX_TEXT_UNITS
                || out
                    .source_offsets
                    .len()
                    .saturating_add(text.encode_utf16().count())
                    > MAX_TEXT_UNITS
            {
                return Err(Error::action("Semantic text exceeds output bound"));
            }
            match part {
                Part::Literal { mapped } => out.append(mapped),
                Part::Url { display, .. } => out.syntax(display),
            }
        }
        Ok(out)
    }
    /// Original 100620dfc only revisits the URL enum case. Rebuilding the map
    /// here preserves generated offsets when replacement display lengths differ.
    pub fn retrim(&mut self, urls: &mut Shortener) -> Result<bool> {
        self.mapped()?;
        let mut changed = false;
        for part in &mut self.parts {
            if let Part::Url {
                original,
                is_image,
                display,
            } = part
            {
                let next = urls.trim(original, *is_image).unwrap_or_default();
                check_urls(urls)?;
                if !canonical_equal(display, &next) {
                    *display = next;
                    changed = true;
                }
            }
        }
        self.mapped()?;
        Ok(changed)
    }
}

fn canonical_equal(left: &str, right: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let left = crate::selection::native_string(left).precomposedStringWithCanonicalMapping();
        let right = crate::selection::native_string(right).precomposedStringWithCanonicalMapping();
        left.isEqualToString(&right)
    }
    #[cfg(not(target_os = "macos"))]
    {
        left == right
    }
}

pub fn check_urls(urls: &Shortener) -> Result<()> {
    if urls.budget_exhausted() {
        Err(Error::action(
            "Native URL preparation exceeded retained-input bounds",
        ))
    } else {
        Ok(())
    }
}

/// The recovered FullUI attribute builder shortens before value deduplication,
/// emits a sole ordinary value bare, and preserves short-before-long ordering.
/// Unknown protocol collections remain absent; their values are not invented.
pub fn attributes(node: &Node, urls: &mut Shortener) -> Result<String> {
    let mut attributes: Vec<(Option<&str>, String)> = Vec::new();
    let mut add =
        |label: Option<&'static str>, value: Option<&str>, keep_empty: bool| -> Result<()> {
            let Some(value) = value.filter(|v| keep_empty || !v.is_empty()) else {
                return Ok(());
            };
            if value.len() > 4 * MAX_TEXT_UNITS || value.encode_utf16().count() > MAX_TEXT_UNITS {
                return Err(Error::action("Native attribute exceeds rendering bound"));
            }
            if !attributes
                .iter()
                .any(|(_, old)| canonical_equal(old, value))
            {
                attributes.push((label, value.to_owned()));
            }
            Ok(())
        };
    add(None, node.title.as_deref(), false)?;
    add(Some("Description"), node.description.as_deref(), false)?;
    if let Some(url) = &node.url {
        let display = urls.trim(url, node.role == "AXImage");
        check_urls(urls)?;
        add(Some("URL"), display.as_deref(), true)?;
    }
    add(Some("Help"), node.help.as_deref(), false)?;
    add(
        Some("Value"),
        node.mapped_value
            .as_ref()
            .map(|m| m.text.as_str())
            .or(node.value.as_deref()),
        false,
    )?;
    add(Some("Details"), node.value_description.as_deref(), false)?;
    add(Some("Placeholder"), node.placeholder.as_deref(), false)?;
    add(
        Some("ID"),
        node.identifier
            .as_deref()
            .filter(|id| !id.starts_with('_') && !id.starts_with("NS")),
        false,
    )?;
    let actions = node
        .actions
        .iter()
        .map(|a| crate::ax::action_label(a))
        .collect::<Vec<_>>()
        .join(", ");
    add(Some("Secondary Actions"), Some(&actions), false)?;
    if attributes.len() == 1 && attributes[0].0 != Some("Secondary Actions") {
        return Ok(attributes.remove(0).1);
    }
    attributes
        .sort_by_key(|(_, value)| value.contains('\n') || value.graphemes(true).nth(100).is_some());
    let mut text = String::new();
    for (label, value) in attributes {
        let extra =
            value.len() + label.map_or(0, |s| s.len() + 2) + usize::from(!text.is_empty()) * 2;
        if text.len().saturating_add(extra) > 4 * MAX_TEXT_UNITS {
            return Err(Error::action("Native attributes exceed output bound"));
        }
        if !text.is_empty() {
            text.push_str(", ");
        }
        if let Some(label) = label {
            text.push_str(label);
            text.push_str(": ");
        }
        text.push_str(&value);
    }
    if text.encode_utf16().count() > MAX_TEXT_UNITS {
        return Err(Error::action("Native attributes exceed UTF16 output bound"));
    }
    Ok(text)
}

/// FullUI preparation corresponds to 1002264c8 followed by 100224bb0's late
/// compaction and the revision's main/focus rendering closures. The callback
/// retrieves/renders attributed data once per retained node visit, never during
/// retrimming or immutable Node::text calls.
pub fn prepare_full_ui(
    mut main: Node,
    mut focus: Option<Box<Node>>,
    app: &str,
    urls: &mut Shortener,
    attributed: &mut impl FnMut(&mut Node, &mut Shortener) -> Result<()>,
) -> Result<Node> {
    fn validate(node: &Node, depth: usize, count: &mut usize) -> Result<()> {
        *count += 1;
        if depth >= 100 || *count > 10000 {
            return Err(Error::action(
                "Native render preparation exceeds tree bound",
            ));
        }
        if let Some(mapped) = &node.mapped_value
            && (mapped.text.encode_utf16().count() != mapped.source_offsets.len()
                || mapped.source_offsets.len() > MAX_TEXT_UNITS)
        {
            return Err(Error::action(
                "Native render preparation has an invalid source map",
            ));
        }
        if let Some(semantic) = &node.semantic_value {
            semantic.mapped()?;
        }
        for child in &node.children {
            validate(child, depth + 1, count)?;
        }
        Ok(())
    }
    fn each(node: &mut Node, operation: &mut impl FnMut(&mut Node) -> Result<()>) -> Result<()> {
        operation(node)?;
        for child in &mut node.children {
            each(child, operation)?;
        }
        Ok(())
    }
    let mut count = 0;
    validate(&main, 0, &mut count)?;
    if let Some(focus) = &focus {
        validate(focus, 0, &mut count)?;
    }
    urls.reset();
    let before = urls.configuration().clone();
    main = crate::ax::transform::apply_with_attributed(
        main,
        app,
        crate::ax::transform::Context::FullUi,
        urls,
        attributed,
    )?;
    if let Some(tree) = focus.take() {
        focus = Some(Box::new(crate::ax::transform::apply_with_attributed(
            *tree,
            app,
            crate::ax::transform::Context::FullUi,
            urls,
            attributed,
        )?));
    }
    let mut collect = |node: &mut Node| attributes(node, urls).map(|_| ());
    each(&mut main, &mut collect)?;
    if let Some(tree) = &mut focus {
        each(tree, &mut collect)?;
    }
    check_urls(urls)?;
    urls.compact_if_needed();
    check_urls(urls)?;
    let changed = &before != urls.configuration();
    urls.reset();
    if changed {
        let mut retrim = |node: &mut Node| {
            if let Some(semantic) = &mut node.semantic_value
                && semantic.retrim(urls)?
            {
                let mapped = semantic.mapped()?;
                node.value = Some(mapped.text.clone());
                node.mapped_value = Some(mapped);
            }
            Ok(())
        };
        each(&mut main, &mut retrim)?;
        if let Some(tree) = &mut focus {
            each(tree, &mut retrim)?;
        }
    }
    // No reset here: recovered revision render closures share the context left
    // by retrim. Cache attributes once so repeated diff/format calls are pure.
    let mut output_bytes = 0usize;
    let mut attributed_source_bytes = 0usize;
    let mut publish = |node: &mut Node| {
        attributed_source_bytes = attributed_source_bytes
            .saturating_add(node.attributed_source.as_ref().map_or(0, String::len));
        if attributed_source_bytes > 16 * 1024 * 1024 {
            return Err(Error::action(
                "Native render preparation exceeds retained selection source bound",
            ));
        }
        let text = attributes(node, urls)?;
        output_bytes = output_bytes.saturating_add(text.len());
        if output_bytes > 8 * 1024 * 1024 {
            return Err(Error::action(
                "Native render preparation exceeds observation output bound",
            ));
        }
        node.prepared_attributes = Some(text);
        Ok(())
    };
    each(&mut main, &mut publish)?;
    if let Some(tree) = &mut focus {
        each(tree, &mut publish)?;
    }
    check_urls(urls)?;
    main.focus_tree = focus;
    Ok(main)
}
