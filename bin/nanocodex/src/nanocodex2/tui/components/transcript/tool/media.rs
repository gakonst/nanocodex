// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{Presentation, format_bytes};
use crate::tui::{format::shorten_home, theme::Theme, transcript::ToolEntry};
use ratatui::style::Style;
use serde_json::Value;
use std::path::Path;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    if tool.family() == "view_image" {
        return view_image(tool, width, theme, expanded);
    }
    image_generation(tool, width, theme, expanded)
}

fn view_image(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let path = tool
        .arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("<image>");
    let detail = tool
        .arguments
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("high");
    let subject = format!("{} · {detail}", shorten_home(Path::new(path)));
    let presentation = Presentation::new("Image", subject);
    if !expanded {
        return presentation;
    }
    let size = tool
        .result
        .as_ref()
        .map_or(0, |result| result.to_string().len());
    let details = super::super::markdown::wrap_plain(
        &format!("image returned · {}", format_bytes(size)),
        width,
        Style::default().fg(theme.accent()),
    );
    presentation
        .unselectable_details(details)
        .footer("binary data hidden")
}

fn image_generation(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let prompt = tool
        .arguments
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("<prompt unavailable>");
    let presentation = Presentation::new(
        "Image generation",
        prompt.lines().next().unwrap_or_default(),
    );
    if !expanded {
        return presentation;
    }
    let mut details =
        super::super::markdown::wrap_plain(prompt, width, Style::default().fg(theme.text()));
    if let Some(paths) = tool
        .arguments
        .get("referenced_image_paths")
        .and_then(Value::as_array)
    {
        details.extend(super::super::markdown::wrap_plain(
            &format!("{} referenced images", paths.len()),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    if tool.result.is_some() {
        details.extend(super::super::markdown::wrap_plain(
            "generated image returned",
            width,
            Style::default().fg(theme.accent()),
        ));
    }
    presentation
        .unselectable_details(details)
        .footer("image generation details")
}
