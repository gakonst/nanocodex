use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Text,
    CursorBefore,
    CursorAfter,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct TextRange {
    pub location: usize,
    pub length: usize,
}

/// UTF-16 source ranges with overlapping matches and adjacent context.
/// macOS uses Foundation search options 0, preserving canonical-equivalent
/// matching and native UTF-16 ranges. Other targets use exact UTF-16 matching.
pub fn select(
    source: &str,
    needle: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
    mode: Mode,
) -> Result<TextRange> {
    let matches = match_ranges(source, needle);
    let source: Vec<u16> = source.encode_utf16().collect();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let prefix: Vec<u16> = prefix.unwrap_or_default().encode_utf16().collect();
    let suffix: Vec<u16> = suffix.unwrap_or_default().encode_utf16().collect();
    if needle.is_empty() {
        return Err(not_found());
    }
    let mut found = None;
    for candidate in matches {
        let start = candidate.location;
        let end = start + candidate.length;
        if start < prefix.len() || !context_equal(&source[start - prefix.len()..start], &prefix) {
            continue;
        }
        if end + suffix.len() > source.len()
            || !context_equal(&source[end..end + suffix.len()], &suffix)
        {
            continue;
        }
        if found.is_some() {
            return Err(not_found());
        }
        found = Some(TextRange {
            location: start,
            length: candidate.length,
        });
    }
    let mut range = found.ok_or_else(not_found)?;
    match mode {
        Mode::Text => {}
        Mode::CursorBefore => range.length = 0,
        Mode::CursorAfter => {
            range.location += range.length;
            range.length = 0;
        }
    }
    Ok(range)
}
#[cfg(target_os = "macos")]
pub(crate) fn native_string(value: &str) -> objc2::rc::Retained<objc2_foundation::NSString> {
    use objc2::AnyThread;
    use objc2_foundation::NSString;
    // NSString's UTF8 byte initializer consumes a leading BOM. Swift's String
    // bridge preserves it, and native source offsets must preserve every unit.
    let mut units: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        NSString::initWithCharacters_length(
            NSString::alloc(),
            std::ptr::NonNull::new(units.as_mut_ptr()).expect("Vec pointer is nonnull"),
            units.len(),
        )
    }
}
#[cfg(target_os = "macos")]
fn context_equal(left: &[u16], right: &[u16]) -> bool {
    if left == right {
        return true;
    }
    // Native NSString lengths are measured before Swift String equality, which
    // preserves canonical equivalence when comparing the extracted context.
    let left =
        native_string(&String::from_utf16_lossy(left)).precomposedStringWithCanonicalMapping();
    let right =
        native_string(&String::from_utf16_lossy(right)).precomposedStringWithCanonicalMapping();
    left.isEqualToString(&right)
}
#[cfg(not(target_os = "macos"))]
fn context_equal(left: &[u16], right: &[u16]) -> bool {
    left == right
}
#[cfg(target_os = "macos")]
fn match_ranges(source: &str, needle: &str) -> Vec<TextRange> {
    use objc2_foundation::{NSRange, NSStringCompareOptions};
    if needle.is_empty() {
        return vec![];
    }
    let source = native_string(source);
    let needle = native_string(needle);
    let mut start = 0;
    let mut found = Vec::new();
    while start < source.length() {
        let range = source.rangeOfString_options_range(
            &needle,
            NSStringCompareOptions::empty(),
            NSRange::new(start, source.length() - start),
        );
        if range.location >= source.length() || range.length == 0 {
            break;
        }
        found.push(TextRange {
            location: range.location,
            length: range.length,
        });
        start = range.location + 1;
    }
    found
}
#[cfg(not(target_os = "macos"))]
fn match_ranges(source: &str, needle: &str) -> Vec<TextRange> {
    let source: Vec<_> = source.encode_utf16().collect();
    let needle: Vec<_> = needle.encode_utf16().collect();
    if needle.is_empty() || needle.len() > source.len() {
        return vec![];
    }
    (0..=source.len() - needle.len())
        .filter(|start| source[*start..*start + needle.len()] == needle)
        .map(|location| TextRange {
            location,
            length: needle.len(),
        })
        .collect()
}

fn not_found() -> Error {
    Error::action("Could not find the requested text to select in the element")
}

pub fn replace_utf16(source: &str, range: TextRange, text: &str) -> Result<String> {
    let mut units: Vec<u16> = source.encode_utf16().collect();
    let end = range
        .location
        .checked_add(range.length)
        .filter(|end| *end <= units.len())
        .ok_or_else(|| Error::invalid("Text range out of bounds"))?;
    units.splice(range.location..end, text.encode_utf16());
    String::from_utf16(&units)
        .map_err(|_| Error::invalid("Text range splits a Unicode surrogate pair"))
}

/// One source offset per rendered UTF-16 code unit. Formatting punctuation has
/// no source offset. The boundary lookup therefore cannot select Markdown syntax.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MappedText {
    pub text: String,
    pub source_offsets: Vec<Option<usize>>,
}
impl MappedText {
    pub fn plain(text: &str, source_start: usize) -> Self {
        Self {
            text: text.into(),
            source_offsets: (source_start..source_start + text.encode_utf16().count())
                .map(Some)
                .collect(),
        }
    }
    pub fn syntax(&mut self, text: &str) {
        self.text.push_str(text);
        self.source_offsets
            .extend(text.encode_utf16().map(|_| None));
    }
    pub fn append(&mut self, other: &Self) {
        self.text.push_str(&other.text);
        self.source_offsets.extend_from_slice(&other.source_offsets);
    }
    pub fn source_range(&self, range: TextRange) -> Result<TextRange> {
        if self.source_offsets.len() != self.text.encode_utf16().count() {
            return Err(Error::action("Rendered text source map has invalid length"));
        }
        let end = range
            .location
            .checked_add(range.length)
            .filter(|end| *end <= self.source_offsets.len())
            .ok_or_else(|| Error::invalid("Rendered range out of bounds"))?;
        let offsets: Vec<_> = self.source_offsets[range.location..end]
            .iter()
            .flatten()
            .copied()
            .collect();
        let (Some(first), Some(last)) = (offsets.first(), offsets.last()) else {
            return Err(Error::action("Selected rendering has no source text"));
        };
        if !offsets.windows(2).all(|w| w[1] == w[0] + 1) {
            return Err(Error::action(
                "Selected rendering crosses disjoint source ranges",
            ));
        }
        Ok(TextRange {
            location: *first,
            length: last - first + 1,
        })
    }
}

/// Mapped text is attempted first; the raw attributed source takes precedence
/// over ordinary value selection in the fallback. Generated display syntax must
/// never become native offsets merely because mapped selection failed.
/// Mode is applied after mapping, since Markdown syntax has no native offset.
pub fn select_node(
    node: &crate::ax::Node,
    needle: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
    mode: Mode,
) -> Result<TextRange> {
    if let Some(range) = node.truncation_range {
        crate::native::text_source::validate_range(range)?;
    }
    if let Some(mapped) = &node.mapped_value
        && let Ok(rendered) = select(&mapped.text, needle, prefix, suffix, Mode::Text)
        && let Ok(range) = mapped.source_range(rendered)
    {
        return finish_node_range(node, range, mode);
    }
    let range = select(
        node.attributed_source
            .as_deref()
            .or(node.value.as_deref())
            .unwrap_or_default(),
        needle,
        prefix,
        suffix,
        Mode::Text,
    )?;
    finish_node_range(node, range, mode)
}
fn finish_node_range(
    node: &crate::ax::Node,
    mut range: TextRange,
    mode: Mode,
) -> Result<TextRange> {
    if let Some(source) = node.truncation_range {
        if range
            .location
            .checked_add(range.length)
            .is_none_or(|end| end > source.length)
        {
            return Err(Error::action(
                "Selection exceeds local truncated source range",
            ));
        }
        range.location = source
            .location
            .checked_add(range.location)
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or_else(|| Error::action("Selection source origin overflow"))?;
    }
    let end = range
        .location
        .checked_add(range.length)
        .filter(|n| *n <= isize::MAX as usize)
        .ok_or_else(|| Error::action("Selection source range overflow"))?;
    match mode {
        Mode::Text => (),
        Mode::CursorBefore => range.length = 0,
        Mode::CursorAfter => {
            range.location = end;
            range.length = 0;
        }
    }
    Ok(range)
}
