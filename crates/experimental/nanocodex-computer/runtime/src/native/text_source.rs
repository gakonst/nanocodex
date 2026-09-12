//! Native source preparation recovered from 10072d714/100730788 and tag11.
//! Provider calls are explicit so partial-value and optional-read paths can be
//! validated without accessibility access or executing the original service.
use crate::{Error, Result, selection::TextRange};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_SOURCE_UNITS: usize = 4 * 1024 * 1024;
pub const MAX_RANGE_UNITS: usize = 1024 * 1024;
pub const TRUNCATION_THRESHOLD: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialValue {
    pub text: String,
    pub truncation: Option<Truncation>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Truncation {
    pub total_count: usize,
    pub available: Option<TextRange>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedValue {
    pub text: String,
    /// Global native range represented by local text; never a selection range.
    pub range: Option<TextRange>,
}
impl PreparedValue {
    pub fn apply_to(self, node: &mut crate::ax::Node) {
        node.value = Some(self.text);
        node.truncation_range = self.range;
        node.attributed_source = None;
        node.mapped_value = None;
        node.semantic_value = None;
        node.prepared_attributes = None;
        if self.range.is_some() {
            node.settable = false;
        }
    }
}

/// Missing/unsupported/ordinary optional provider errors are `None` after the
/// adapter records diagnostics. Identity, deadline, type invariants and resource
/// failures remain `Err` and are never converted into successful preparation.
pub trait Provider {
    fn check(&mut self) -> Result<()>;
    fn visible_range(&mut self) -> Result<Option<TextRange>>;
    fn string_for_range(&mut self, range: TextRange) -> Result<Option<String>>;
    fn refresh_value(&mut self) -> Result<Option<String>>;
    fn textual_context(&mut self) -> Result<Option<String>>;
}

pub fn validate_range(range: TextRange) -> Result<()> {
    if range.length > MAX_RANGE_UNITS
        || range.location > isize::MAX as usize
        || range
            .location
            .checked_add(range.length)
            .is_none_or(|end| end > isize::MAX as usize)
    {
        return Err(Error::action("Native text range exceeds bounds"));
    }
    Ok(())
}
pub fn attributed_range(source: &str, requested: Option<TextRange>) -> Result<TextRange> {
    checked_text(source)?;
    let length = source.encode_utf16().count();
    let range = requested.unwrap_or(TextRange {
        location: 0,
        length,
    });
    validate_range(range)?;
    if range.length != length {
        return Err(Error::action(
            "Attributed source length differs from requested native range",
        ));
    }
    Ok(range)
}
fn checked_text(text: &str) -> Result<()> {
    if text.len() > MAX_SOURCE_UNITS * 4 || text.encode_utf16().count() > MAX_SOURCE_UNITS {
        Err(Error::action("Native text source exceeds retention bound"))
    } else {
        Ok(())
    }
}
pub fn attributed_allowed(cached: Option<&str>, provider: &mut impl Provider) -> Result<bool> {
    provider.check()?;
    let live;
    let context = if let Some(value) = cached {
        Some(value)
    } else {
        live = provider.textual_context()?;
        live.as_deref()
    };
    provider.check()?;
    if context.is_some_and(|s| s.len() > 16 * 1024) {
        return Err(Error::action("AXTextualContext exceeds metadata bound"));
    }
    Ok(context != Some("AXTextualContextSourceCode"))
}

/// Swift's Range(NSRange,in:String) checks UTF16 bounds; String slicing rounds
/// an interior-surrogate String.Index down to the scalar's beginning. Pinned
/// Foundation oracle cases distinguish this from NSString substring behavior.
pub fn local_slice(source: &str, range: TextRange) -> Option<String> {
    checked_text(source).ok()?;
    let end = range.location.checked_add(range.length)?;
    let units: Vec<_> = source.encode_utf16().collect();
    units.get(range.location..end)?;
    let align = |index: usize| {
        if index > 0
            && index < units.len()
            && (0xdc00..=0xdfff).contains(&units[index])
            && (0xd800..=0xdbff).contains(&units[index - 1])
        {
            index - 1
        } else {
            index
        }
    };
    String::from_utf16(&units[align(range.location)..align(end)]).ok()
}
pub fn string_for_range(
    source: &str,
    range: TextRange,
    provider: &mut impl Provider,
) -> Result<String> {
    checked_text(source)?;
    validate_range(range)?;
    provider.check()?;
    // The original compares Swift graphemes to an NSRange end, not UTF16 length.
    let result = if source.graphemes(true).count() < range.location + range.length {
        provider.string_for_range(range)?.unwrap_or_default()
    } else {
        local_slice(source, range).unwrap_or_default()
    };
    provider.check()?;
    checked_text(&result)?;
    if result.encode_utf16().count() > MAX_RANGE_UNITS {
        return Err(Error::action("Native range result exceeds output bound"));
    }
    Ok(result)
}
pub fn prepare(
    role: &str,
    input: PartialValue,
    provider: &mut impl Provider,
) -> Result<PreparedValue> {
    checked_text(&input.text)?;
    provider.check()?;
    if !matches!(role, "AXTextArea" | "AXTextField" | "AXStaticText") {
        return Ok(PreparedValue {
            text: input.text,
            range: None,
        });
    }
    let total = input
        .truncation
        .map_or_else(|| input.text.encode_utf16().count(), |t| t.total_count);
    if total > isize::MAX as usize {
        return Err(Error::action("Native partial text count exceeds bounds"));
    }
    if let Some(range) = input.truncation.and_then(|t| t.available)
        && range
            .location
            .checked_add(range.length)
            .is_none_or(|end| end > total)
    {
        return Err(Error::action(
            "Native partial text range exceeds total count",
        ));
    }
    if input.text.graphemes(true).count().max(total) > TRUNCATION_THRESHOLD {
        let range = provider
            .visible_range()?
            .filter(|r| r.length > 0)
            .unwrap_or(TextRange {
                location: 0,
                length: TRUNCATION_THRESHOLD,
            });
        provider.check()?;
        validate_range(range)?;
        let text = string_for_range(&input.text, range, provider)?;
        return Ok(PreparedValue {
            text,
            range: Some(range),
        });
    }
    if let Some(range) = input.truncation.and_then(|t| t.available)
        && range.length < total
    {
        let refreshed = provider.refresh_value()?;
        provider.check()?;
        if let Some(text) = refreshed {
            checked_text(&text)?;
            return Ok(PreparedValue { text, range: None });
        }
        return Ok(PreparedValue {
            text: input.text,
            range: Some(range),
        });
    }
    Ok(PreparedValue {
        text: input.text,
        range: None,
    })
}
