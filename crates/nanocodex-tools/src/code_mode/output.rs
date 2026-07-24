use crate::ToolOutputContent;

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

pub(super) fn truncate_content(
    content: Vec<ToolOutputContent>,
    max_output_tokens: Option<usize>,
) -> Vec<ToolOutputContent> {
    let max_output_tokens = max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    if content
        .iter()
        .all(|item| matches!(item, ToolOutputContent::InputText { .. }))
    {
        return truncate_text_only(content, max_output_tokens);
    }

    truncate_mixed(content, max_output_tokens)
}

fn truncate_text_only(
    content: Vec<ToolOutputContent>,
    max_output_tokens: usize,
) -> Vec<ToolOutputContent> {
    let text = content
        .iter()
        .filter_map(|item| match item {
            ToolOutputContent::InputText { text } => Some(text.as_str()),
            ToolOutputContent::InputImage { .. } | ToolOutputContent::InputAudio { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let byte_budget = max_output_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= byte_budget {
        return content;
    }

    let original_token_count = approx_tokens(text.len());
    let total_lines = text.lines().count();
    let truncated = truncate_middle_tokens(&text, max_output_tokens);
    vec![ToolOutputContent::InputText {
        text: format!(
            "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {total_lines}\n\n{truncated}"
        ),
    }]
}

fn truncate_mixed(
    content: Vec<ToolOutputContent>,
    max_output_tokens: usize,
) -> Vec<ToolOutputContent> {
    let mut output = Vec::with_capacity(content.len());
    let mut remaining = max_output_tokens;
    let mut omitted_text_items = 0_usize;
    for item in content {
        match item {
            ToolOutputContent::InputText { text } => {
                if remaining == 0 {
                    omitted_text_items = omitted_text_items.saturating_add(1);
                    continue;
                }
                let cost = approx_tokens(text.len());
                if cost <= remaining {
                    output.push(ToolOutputContent::InputText { text });
                    remaining = remaining.saturating_sub(cost);
                } else {
                    let snippet = truncate_middle_tokens(&text, remaining);
                    if snippet.is_empty() {
                        omitted_text_items = omitted_text_items.saturating_add(1);
                    } else {
                        output.push(ToolOutputContent::InputText { text: snippet });
                    }
                    remaining = 0;
                }
            }
            image @ ToolOutputContent::InputImage { .. } => output.push(image),
            ToolOutputContent::InputAudio { .. } => {}
        }
    }
    if omitted_text_items > 0 {
        output.push(ToolOutputContent::InputText {
            text: format!("[omitted {omitted_text_items} text items ...]"),
        });
    }
    output
}

fn truncate_middle_tokens(text: &str, max_tokens: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let byte_budget = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if max_tokens > 0 && text.len() <= byte_budget {
        return text.to_owned();
    }
    truncate_middle(text, byte_budget, true)
}

fn truncate_middle(text: &str, byte_budget: usize, use_tokens: bool) -> String {
    if text.is_empty() {
        return String::new();
    }
    if byte_budget == 0 {
        return truncation_marker(use_tokens, text.len(), text.chars().count());
    }
    if text.len() <= byte_budget {
        return text.to_owned();
    }
    let left_budget = byte_budget / 2;
    let right_budget = byte_budget.saturating_sub(left_budget);
    let prefix_end = floor_char_boundary(text, left_budget);
    let suffix_start =
        ceil_char_boundary(text, text.len().saturating_sub(right_budget)).max(prefix_end);
    let removed = &text[prefix_end..suffix_start];
    let marker = truncation_marker(use_tokens, removed.len(), removed.chars().count());
    format!("{}{marker}{}", &text[..prefix_end], &text[suffix_start..])
}

fn truncation_marker(use_tokens: bool, removed_bytes: usize, removed_chars: usize) -> String {
    if use_tokens {
        format!("…{} tokens truncated…", approx_tokens(removed_bytes))
    } else {
        format!("…{removed_chars} chars truncated…")
    }
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

fn floor_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

fn ceil_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_add(1);
    }
    boundary
}

#[cfg(test)]
mod tests {
    use crate::ToolOutputContent;

    use super::truncate_content;

    #[test]
    fn truncates_text_and_preserves_images() {
        let content = vec![
            ToolOutputContent::InputText {
                text: "abcdefgh".to_owned(),
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,a".to_owned(),
                detail: crate::ImageDetail::High,
            },
            ToolOutputContent::InputText {
                text: "ijklmnop".to_owned(),
            },
        ];
        let output = truncate_content(content, Some(2));
        assert!(matches!(
            &output[0],
            ToolOutputContent::InputText { text } if text == "abcdefgh"
        ));
        assert!(matches!(output[1], ToolOutputContent::InputImage { .. }));
        assert!(matches!(
            &output[2],
            ToolOutputContent::InputText { text } if text == "[omitted 1 text items ...]"
        ));
    }

    #[test]
    fn merges_and_formats_text_only_truncation() {
        let output = truncate_content(
            vec![
                ToolOutputContent::InputText {
                    text: "abcdefgh".to_owned(),
                },
                ToolOutputContent::InputText {
                    text: "ijklmnop".to_owned(),
                },
            ],
            Some(2),
        );
        assert!(matches!(
            &output[0],
            ToolOutputContent::InputText { text }
                if text == "Warning: truncated output (original token count: 5)\nTotal output lines: 2\n\nabcd…3 tokens truncated…mnop"
        ));
    }
}
