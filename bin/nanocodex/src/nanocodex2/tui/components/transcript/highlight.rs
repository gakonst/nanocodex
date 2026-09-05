// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Shared, cached syntax highlighting for transcript code.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use std::{path::Path, str::FromStr, sync::OnceLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SyntectColor, FontStyle, ScopeSelectors, StyleModifier, Theme as SyntaxTheme,
        ThemeItem, ThemeSettings,
    },
    parsing::{SyntaxReference, SyntaxSet},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct Assets {
    pub(super) syntaxes: SyntaxSet,
}

pub(super) fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| Assets {
        syntaxes: SyntaxSet::load_defaults_newlines(),
    })
}

pub(super) fn theme() -> SyntaxTheme {
    SyntaxTheme {
        name: Some("tact".to_owned()),
        settings: ThemeSettings {
            foreground: Some(syntect_color(Color::Reset)),
            accent: Some(syntect_color(Color::Blue)),
            ..ThemeSettings::default()
        },
        scopes: vec![
            rule("comment", Color::DarkGray, Some(FontStyle::ITALIC)),
            rule("string", Color::Green, None),
            rule(
                "constant.numeric, constant.language, constant.character",
                Color::Magenta,
                None,
            ),
            rule("keyword, storage", Color::Blue, Some(FontStyle::BOLD)),
            rule("entity.name.function, support.function", Color::Cyan, None),
            rule(
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, support.type",
                Color::Yellow,
                None,
            ),
            rule("variable.parameter", Color::Reset, Some(FontStyle::ITALIC)),
            rule("invalid", Color::Red, Some(FontStyle::UNDERLINE)),
        ],
        ..SyntaxTheme::default()
    }
}

pub(super) fn syntax_for_token<'a>(syntaxes: &'a SyntaxSet, token: &str) -> &'a SyntaxReference {
    let token = token.split_ascii_whitespace().next().unwrap_or_default();
    syntaxes
        .find_syntax_by_token(token)
        .or_else(|| syntaxes.find_syntax_by_extension(token))
        .or_else(|| syntaxes.find_syntax_by_name(token))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
}

pub(super) fn syntax_for_path<'a>(syntaxes: &'a SyntaxSet, path: &str) -> &'a SyntaxReference {
    let path = Path::new(path);
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| syntaxes.find_syntax_by_extension(extension))
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| syntaxes.find_syntax_by_extension(name))
        })
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
}

pub(super) fn line(
    highlighter: &mut HighlightLines<'_>,
    text: &str,
    syntaxes: &SyntaxSet,
) -> Vec<Span<'static>> {
    let line = format!("{text}\n");
    let Ok(regions) = highlighter.highlight_line(&line, syntaxes) else {
        return vec![Span::styled(text.to_owned(), code_style())];
    };
    let mut spans = Vec::new();
    for (style, region) in regions {
        let region = region.trim_end_matches('\n');
        if region.is_empty() {
            continue;
        }
        let mut modifier = Modifier::empty();
        if style.font_style.contains(FontStyle::BOLD) {
            modifier.insert(Modifier::BOLD);
        }
        if style.font_style.contains(FontStyle::ITALIC) {
            modifier.insert(Modifier::ITALIC);
        }
        if style.font_style.contains(FontStyle::UNDERLINE) {
            modifier.insert(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(
            region.to_owned(),
            Style::default()
                .fg(terminal_color(style.foreground))
                .add_modifier(modifier),
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), code_style()));
    }
    spans
}

pub(super) fn code_style() -> Style {
    Style::default().fg(Color::Reset)
}

pub(super) fn wrap(spans: Vec<Span<'static>>, width: u16) -> Vec<Vec<Span<'static>>> {
    let mut lines = vec![Vec::<Span<'static>>::new()];
    let mut used = 0_u16;
    for span in spans {
        for grapheme in span.content.graphemes(true) {
            let grapheme_width =
                u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(u16::MAX);
            if grapheme_width > width {
                if used > 0 {
                    lines.push(Vec::new());
                }
                push_grapheme(
                    lines.last_mut().expect("a line always exists"),
                    "�",
                    span.style,
                );
                used = 1;
                continue;
            }
            if used.saturating_add(grapheme_width) > width && used > 0 {
                lines.push(Vec::new());
                used = 0;
            }
            push_grapheme(
                lines.last_mut().expect("a line always exists"),
                grapheme,
                span.style,
            );
            used = used.saturating_add(grapheme_width);
        }
    }
    lines
}

fn push_grapheme(spans: &mut Vec<Span<'static>>, grapheme: &str, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(grapheme);
        return;
    }
    spans.push(Span::styled(grapheme.to_owned(), style));
}

fn rule(scope: &str, color: Color, font_style: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope).expect("built-in syntax scopes should be valid"),
        style: StyleModifier {
            foreground: Some(syntect_color(color)),
            background: None,
            font_style,
        },
    }
}

fn syntect_color(color: Color) -> SyntectColor {
    // Syntect accepts only RGB colors. These values are internal identifiers that are converted
    // back to named Ratatui colors before rendering, leaving the terminal palette authoritative.
    let (r, g, b) = match color {
        Color::Reset => (0xd7, 0xd7, 0xd7),
        Color::Red => (0xcd, 0x31, 0x31),
        Color::Green => (0x0d, 0xbc, 0x79),
        Color::Yellow => (0xe5, 0xe5, 0x10),
        Color::Blue => (0x24, 0x72, 0xc8),
        Color::Magenta => (0xbc, 0x3f, 0xbc),
        Color::Cyan => (0x11, 0xa8, 0xcd),
        Color::DarkGray => (0x66, 0x66, 0x66),
        _ => unreachable!("syntax themes use named terminal colors"),
    };
    SyntectColor { r, g, b, a: 0xff }
}

fn terminal_color(color: SyntectColor) -> Color {
    match (color.r, color.g, color.b) {
        (0xd7, 0xd7, 0xd7) => Color::Reset,
        (0xcd, 0x31, 0x31) => Color::Red,
        (0x0d, 0xbc, 0x79) => Color::Green,
        (0xe5, 0xe5, 0x10) => Color::Yellow,
        (0x24, 0x72, 0xc8) => Color::Blue,
        (0xbc, 0x3f, 0xbc) => Color::Magenta,
        (0x11, 0xa8, 0xcd) => Color::Cyan,
        (0x66, 0x66, 0x66) => Color::DarkGray,
        _ => Color::Reset,
    }
}
