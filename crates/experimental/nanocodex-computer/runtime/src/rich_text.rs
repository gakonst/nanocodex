//! Pure Rust conversion: no WebKit, URL loading, CSS evaluation or native HTML
//! importer. The supported formatting subset is intentionally explicit.
use crate::{Error, Result, clipboard::Item};
use scraper::{Html, Node};

pub fn representations(text: &str, format: &str) -> Result<Item> {
    if text.len() > 1024 * 1024 {
        return Err(Error::invalid("Paste content exceeds 1 MiB"));
    }
    let mut item = Item::new();
    if format == "text" {
        item.insert("public.utf8-plain-text".into(), text.as_bytes().to_vec());
        return Ok(item);
    }
    let source = match format {
        "html" => text.to_owned(),
        "md" => {
            let mut html = String::new();
            pulldown_cmark::html::push_html(
                &mut html,
                pulldown_cmark::Parser::new_ext(
                    text,
                    pulldown_cmark::Options::ENABLE_STRIKETHROUGH
                        | pulldown_cmark::Options::ENABLE_TABLES,
                ),
            );
            html
        }
        _ => return Err(Error::invalid("Unknown paste format")),
    };
    let document = Html::parse_fragment(&source);
    let mut stack = vec![(document.tree.root(), false, 0usize, 0usize)];
    let mut plain = String::new();
    let mut html = String::new();
    let mut rtf = String::from("{\\rtf1\\ansi\\uc1 ");
    let mut count = 0;
    while let Some((node, closing, depth, pre)) = stack.pop() {
        if depth > 100 {
            return Err(Error::invalid("HTML nesting exceeds 100"));
        }
        if !closing {
            count += 1;
            if count > 50000 {
                return Err(Error::invalid("HTML node limit exceeded"));
            }
        }
        match node.value() {
            Node::Text(t) if !closing => {
                for ch in t.text.chars() {
                    if pre == 0 && ch.is_whitespace() {
                        if !plain.is_empty() && !plain.ends_with(char::is_whitespace) {
                            append(" ", &mut plain, &mut html, &mut rtf);
                        }
                    } else {
                        append(&ch.to_string(), &mut plain, &mut html, &mut rtf);
                    }
                }
            }
            Node::Element(e) => {
                let tag = e.name();
                if matches!(
                    tag,
                    "script"
                        | "style"
                        | "iframe"
                        | "object"
                        | "embed"
                        | "template"
                        | "noscript"
                        | "svg"
                        | "math"
                        | "head"
                ) {
                    continue;
                }
                let allowed = matches!(
                    tag,
                    "p" | "div"
                        | "span"
                        | "b"
                        | "strong"
                        | "i"
                        | "em"
                        | "u"
                        | "s"
                        | "del"
                        | "pre"
                        | "code"
                        | "br"
                        | "ul"
                        | "ol"
                        | "li"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "blockquote"
                        | "table"
                        | "thead"
                        | "tbody"
                        | "tr"
                        | "td"
                        | "th"
                );
                let block = matches!(
                    tag,
                    "p" | "div"
                        | "pre"
                        | "li"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "blockquote"
                        | "tr"
                );
                if closing {
                    if allowed {
                        html.push_str(&format!("</{tag}>"));
                        rtf.push('}');
                    }
                    if block {
                        newline(&mut plain, &mut rtf);
                    } else if matches!(tag, "td" | "th") {
                        append(" ", &mut plain, &mut html, &mut rtf);
                    }
                    continue;
                }
                if block {
                    newline(&mut plain, &mut rtf);
                }
                if tag == "br" {
                    newline(&mut plain, &mut rtf);
                    html.push_str("<br>");
                    continue;
                }
                if tag == "img" {
                    if let Some(alt) = e.attr("alt") {
                        append(alt, &mut plain, &mut html, &mut rtf);
                    }
                    continue;
                }
                if allowed {
                    html.push_str(&format!("<{tag}>"));
                    rtf.push('{');
                    rtf.push_str(match tag {
                        "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "\\b ",
                        "i" | "em" => "\\i ",
                        "u" => "\\ul ",
                        "s" | "del" => "\\strike ",
                        _ => "",
                    });
                }
                if tag == "li" {
                    append("• ", &mut plain, &mut html, &mut rtf);
                }
                stack.push((node, true, depth, pre));
                let pre = pre + usize::from(tag == "pre");
                for child in node.children().rev() {
                    stack.push((child, false, depth + 1, pre));
                }
            }
            Node::Document | Node::Fragment if !closing => {
                for child in node.children().rev() {
                    stack.push((child, false, depth + 1, pre));
                }
            }
            _ => {}
        }
    }
    rtf.push('}');
    item.insert(
        "public.utf8-plain-text".into(),
        plain.trim_end_matches('\n').as_bytes().to_vec(),
    );
    item.insert("public.html".into(), html.into_bytes());
    item.insert("public.rtf".into(), rtf.into_bytes());
    Ok(item)
}
fn newline(plain: &mut String, rtf: &mut String) {
    if !plain.is_empty() && !plain.ends_with('\n') {
        plain.push('\n');
        rtf.push_str("\\par\n");
    }
}
fn append(s: &str, plain: &mut String, html: &mut String, rtf: &mut String) {
    plain.push_str(s);
    for c in s.chars() {
        match c {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            _ => html.push(c),
        }
    }
    for u in s.encode_utf16() {
        match u {
            10 => rtf.push_str("\\line "),
            13 => {}
            9 => rtf.push_str("\\tab "),
            92 | 123 | 125 => {
                rtf.push('\\');
                rtf.push(char::from_u32(u as u32).unwrap());
            }
            32..=126 => rtf.push(char::from_u32(u as u32).unwrap()),
            _ => rtf.push_str(&format!("\\u{}?", u as i16)),
        }
    }
}

/// Source ranges are native UTF-16 offsets. No URL or attachment is dereferenced.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub fixed_width: bool,
    pub superscript: i8,
    /// Already-shortened URL display, resolved from the corresponding AX element.
    pub link: Option<String>,
    #[serde(default)]
    pub attachment: Option<TextAttachment>,
    #[serde(default)]
    pub heading: u8,
    #[serde(default)]
    pub blockquote: usize,
    #[serde(default)]
    pub list: Option<ListStyle>,
    #[serde(default)]
    pub collapsed: bool,
}
/// Metadata only; an attachment never reads a file or follows a URL. `image_url`
/// is present only when the role-aware native shortener accepted an image URL.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TextAttachment {
    pub role_description: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ListStyle {
    pub level: usize,
    pub index: i64,
    pub marker: String,
}
impl ListStyle {
    /// Recovered AXListItemPrefix classification, including exact checkbox labels.
    pub fn from_accessibility(level: usize, index: i64, prefix: &str) -> Self {
        Self::from_accessibility_index_label(level, index, &index.to_string(), prefix)
    }
    /// The original marker uses NSNumber.stringValue, separately from the
    /// integer index retained for style identity (fractional numbers can differ).
    pub fn from_accessibility_index_label(
        level: usize,
        index: i64,
        label: &str,
        prefix: &str,
    ) -> Self {
        let marker = match prefix {
            "checklist item, incomplete" => "* [ ]".into(),
            "checklist item, completed" => "* [x]".into(),
            _ if prefix.chars().next().is_some_and(char::is_numeric) => format!("{label}."),
            _ => "*".into(),
        };
        Self {
            level,
            index,
            marker,
        }
    }
}
impl TextStyle {
    /// The original switch is case-sensitive and separates names using comma-space.
    /// Body, Contains paragraphs and Expanded do not add rendering delimiters.
    pub fn accessibility_style_names(&mut self, names: &str) {
        for name in names.split(", ") {
            match name {
                "Title" => self.heading = 1,
                "Heading" => self.heading = 2,
                "Subheading" => self.heading = 3,
                "Fixed width" => self.fixed_width = true,
                "Collapsed" => self.collapsed = true,
                _ => {}
            }
        }
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AttributedRun {
    pub range: crate::selection::TextRange,
    pub style: TextStyle,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockStyle {
    heading: u8,
    blockquote: usize,
    list: Option<ListStyle>,
    fixed_width: bool,
    collapsed: bool,
}
impl BlockStyle {
    fn new(style: &TextStyle) -> Self {
        Self {
            heading: style.heading,
            blockquote: style.blockquote,
            list: style.list.clone(),
            fixed_width: style.fixed_width,
            collapsed: style.collapsed,
        }
    }
    fn active(&self) -> bool {
        self.heading > 0
            || self.blockquote > 0
            || self.list.is_some()
            || self.fixed_width
            || self.collapsed
    }
    fn prefix(&self) -> Result<String> {
        if self.heading > 6
            || self.blockquote > 128
            || self
                .list
                .as_ref()
                .is_some_and(|list| list.level > 128 || list.marker.len() > 128)
        {
            return Err(Error::invalid(
                "Attributed paragraph prefix exceeds rendering bounds",
            ));
        }
        let mut out = String::new();
        if let Some(list) = &self.list {
            out.push_str(&" ".repeat(list.level * 4));
            out.push_str(&list.marker);
            out.push(' ');
        }
        if self.heading > 0 {
            out.push_str(&"#".repeat(self.heading as usize));
            out.push(' ');
        }
        if self.blockquote > 0 {
            out.push_str(&">".repeat(self.blockquote));
            out.push(' ');
        }
        Ok(out)
    }
    fn open(&self, out: &mut crate::selection::MappedText) {
        if self.collapsed {
            out.syntax("<details><summary>\n");
        }
        if self.fixed_width {
            out.syntax("```\n");
        }
    }
    fn close(&self, out: &mut crate::selection::MappedText) {
        if self.fixed_width {
            if !out.text.ends_with('\n') {
                out.syntax("\n");
            }
            out.syntax("```");
        }
        if self.collapsed {
            out.syntax("\n</summary>(collapsed content is hidden)</details>");
        }
    }
}
fn inline_styles(style: &TextStyle) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (enabled, opening, closing) in [
        (style.bold, "**", "**"),
        (style.italic, "*", "*"),
        (style.underline, "<u>", "</u>"),
        (style.strikethrough, "~~", "~~"),
        (style.superscript > 0, "<sup>", "</sup>"),
        (style.superscript < 0, "<sub>", "</sub>"),
    ] {
        if enabled {
            out.push((opening.into(), closing.into()));
        }
    }
    out
}
fn trim_mapped(text: &crate::selection::MappedText) -> Result<crate::selection::MappedText> {
    use crate::selection::MappedText;
    #[cfg(target_os = "macos")]
    let (trimmed, range) = {
        use objc2_foundation::NSCharacterSet;
        let source = crate::selection::native_string(&text.text);
        let trimmed = source
            .stringByTrimmingCharactersInSet(&NSCharacterSet::whitespaceAndNewlineCharacterSet());
        let range = source.rangeOfString(&trimmed);
        (
            trimmed.to_string(),
            range
                .location
                .checked_add(range.length)
                .map(|end| range.location..end),
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (trimmed, range) = {
        // Portable fixture implementation; the macOS path uses Foundation's
        // actual trimming and canonical rangeOfString behavior.
        let whitespace = |ch: char| ch.is_whitespace() || ch == '\u{200b}';
        let leading = text.text.trim_start_matches(whitespace);
        let trimmed = leading.trim_end_matches(whitespace);
        let start = text.text[..text.text.len() - leading.len()]
            .encode_utf16()
            .count();
        (
            trimmed.to_owned(),
            Some(start..start + trimmed.encode_utf16().count()),
        )
    };
    if trimmed.is_empty() {
        return Ok(MappedText::default());
    }
    let Some(range) = range.filter(|range| range.start < text.source_offsets.len()) else {
        // The original uses generated offsets when NSString cannot locate the
        // trimmed result. It never fabricates an editable source range.
        let mut generated = MappedText::default();
        generated.syntax(&trimmed);
        return Ok(generated);
    };
    let offsets = text
        .source_offsets
        .get(range)
        .ok_or_else(|| Error::action("Trimmed source range is out of bounds"))?;
    if offsets.len() != trimmed.encode_utf16().count() {
        return Err(Error::action(
            "Trimmed text and source range lengths differ",
        ));
    }
    Ok(MappedText {
        text: trimmed,
        source_offsets: offsets.to_vec(),
    })
}
fn attributed_fragment(
    source: &str,
    start: usize,
    style: &TextStyle,
) -> Result<crate::selection::MappedText> {
    use crate::selection::MappedText;
    let mut text = MappedText::plain(source, start);
    // Retained 10073dd80 constructs link/attachment mapped strings before
    // transitioning ordinary font styles. They are not inline-style members.
    if let Some(url) = &style.link {
        let mut linked = MappedText::default();
        linked.syntax("[");
        linked.append(&trim_mapped(&text)?);
        linked.syntax(&format!("]({url})"));
        text = linked;
    }
    if let Some(attachment) = &style.attachment {
        let multiline = text
            .text
            .chars()
            .any(|ch| matches!(ch, '\u{a}'..='\u{d}' | '\u{85}' | '\u{2028}' | '\u{2029}'));
        let caption =
            (!text.text.is_empty() && text.text != "\u{fffc}" && !multiline).then_some(&text);
        let mut replacement = MappedText::default();
        if let Some(url) = &attachment.image_url {
            replacement.syntax("![");
            let mut alt = MappedText::default();
            alt.syntax(attachment.description.as_deref().unwrap_or_default());
            replacement.append(&trim_mapped(caption.unwrap_or(&alt))?);
            replacement.syntax(&format!("]({url})"));
        } else {
            replacement.syntax("[");
            replacement.syntax(
                attachment
                    .role_description
                    .as_deref()
                    .unwrap_or("attachment"),
            );
            let description = caption
                .map(|text| text.text.as_str())
                .or(attachment.description.as_deref());
            if let Some(description) = description.filter(|text| !text.is_empty()) {
                replacement.syntax(": ");
                // The fallback caption, unlike the image caption, is generated
                // metadata with no editable source ranges in the original.
                replacement.syntax(description);
            }
            replacement.syntax("]");
        }
        if multiline {
            replacement.syntax(" ");
            replacement.append(&text);
        }
        text = replacement;
    }
    Ok(text)
}
/// Preserve source UTF-16 ranges while changing style state only at boundaries.
/// Paragraph/list/code syntax has no editable source offset. Native style names,
/// marker constants and four-space indentation come from the retained renderer.
pub fn render_attributed(
    source: &str,
    runs: &[AttributedRun],
) -> Result<crate::selection::MappedText> {
    use crate::selection::MappedText;
    if source.len() > 4 * 1024 * 1024 || runs.len() > 65536 {
        return Err(Error::invalid("Attributed input exceeds rendering bounds"));
    }
    let units: Vec<_> = source.encode_utf16().collect();
    let mut segments = Vec::new();
    let mut next = 0;
    let plain_style = TextStyle::default();
    for run in runs {
        let attachment = run.style.attachment.as_ref();
        if [
            run.style.link.as_deref(),
            attachment.and_then(|a| a.image_url.as_deref()),
            attachment.and_then(|a| a.description.as_deref()),
            attachment.and_then(|a| a.role_description.as_deref()),
        ]
        .into_iter()
        .flatten()
        .any(|s| s.len() > 64 * 1024)
        {
            return Err(Error::invalid(
                "Attributed metadata exceeds rendering bounds",
            ));
        }
        let end = run
            .range
            .location
            .checked_add(run.range.length)
            .filter(|end| *end <= units.len())
            .ok_or_else(|| Error::invalid("Attributed source range out of bounds"))?;
        if run.range.location < next || run.range.length == 0 {
            return Err(Error::invalid("Overlapping or empty attributed source run"));
        }
        if next < run.range.location {
            segments.push((next, run.range.location, &plain_style));
        }
        // Reject both boundaries before publishing a partial source map.
        String::from_utf16(&units[run.range.location..end])
            .map_err(|_| Error::invalid("Attributed run splits surrogate pair"))?;
        segments.push((run.range.location, end, &run.style));
        next = end;
    }
    if next < units.len() {
        segments.push((next, units.len(), &plain_style));
    }
    let mut out = MappedText::default();
    let mut block = BlockStyle::new(&TextStyle::default());
    let mut inline: Vec<(String, String)> = vec![];
    for (start, end, style) in segments {
        let updated = BlockStyle::new(style);
        let prefix = updated.prefix()?;
        if updated != block {
            for (_, closing) in inline.iter().rev() {
                out.syntax(closing);
            }
            inline.clear();
            block.close(&mut out);
            if (block.active() || updated.active())
                && !out.text.is_empty()
                && !out.text.ends_with('\n')
            {
                out.syntax("\n");
            }
            updated.open(&mut out);
            block = updated;
        }
        let desired = inline_styles(style);
        let shared = inline
            .iter()
            .zip(&desired)
            .take_while(|(a, b)| a == b)
            .count();
        for (_, closing) in inline[shared..].iter().rev() {
            out.syntax(closing);
        }
        inline.truncate(shared);
        let text = String::from_utf16(&units[start..end])
            .map_err(|_| Error::invalid("Attributed range splits a surrogate pair"))?;
        let fragment = attributed_fragment(&text, start, style)?;
        let replaced = style.link.is_some() || style.attachment.is_some();
        let mut offset = 0;
        for ch in fragment.text.chars() {
            if !prefix.is_empty() && (out.text.is_empty() || out.text.ends_with('\n')) {
                out.syntax(&prefix);
            }
            if inline.len() < desired.len() {
                for (opening, _) in &desired[inline.len()..] {
                    out.syntax(opening);
                }
                inline = desired.clone();
            }
            // A paragraph break cannot leave inline delimiters stranded across a prefix.
            if ch == '\n' && block.active() {
                for (_, closing) in inline.iter().rev() {
                    out.syntax(closing);
                }
                inline.clear();
            }
            if !replaced
                && !style.fixed_width
                && matches!(ch, '\\' | '*' | '_' | '[' | ']' | '`' | '~' | '<' | '>')
            {
                out.syntax("\\");
            }
            out.text.push(ch);
            out.source_offsets
                .extend_from_slice(&fragment.source_offsets[offset..offset + ch.len_utf16()]);
            offset += ch.len_utf16();
            if out.source_offsets.len() > 4 * 1024 * 1024 {
                return Err(Error::invalid("Attributed output exceeds rendering bounds"));
            }
        }
        if out.source_offsets.len() > 4 * 1024 * 1024 {
            return Err(Error::invalid("Attributed output exceeds rendering bounds"));
        }
    }
    for (_, closing) in inline.iter().rev() {
        out.syntax(closing);
    }
    block.close(&mut out);
    if out.source_offsets.len() > 4 * 1024 * 1024 {
        return Err(Error::invalid("Attributed output exceeds rendering bounds"));
    }
    Ok(out)
}
