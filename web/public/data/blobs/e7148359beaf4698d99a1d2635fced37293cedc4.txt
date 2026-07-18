use crate::tools::ToolOutputContent;

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

pub(super) fn truncate_content(
    content: Vec<ToolOutputContent>,
    max_output_tokens: Option<usize>,
) -> Vec<ToolOutputContent> {
    let max_output_tokens = max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let text = content
        .iter()
        .filter_map(|item| match item {
            ToolOutputContent::InputText { text } => Some(text.as_str()),
            ToolOutputContent::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let byte_budget = max_output_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= byte_budget {
        return content;
    }

    let original_token_count = approx_tokens(text.len());
    let total_lines = text.lines().count();
    let truncated = truncate_middle(&text, byte_budget);
    let mut output = vec![ToolOutputContent::InputText {
        text: format!(
            "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {total_lines}\n\n{truncated}"
        ),
    }];
    output.extend(
        content
            .into_iter()
            .filter(|item| matches!(item, ToolOutputContent::InputImage { .. })),
    );
    output
}

fn truncate_middle(text: &str, byte_budget: usize) -> String {
    let left_budget = byte_budget / 2;
    let right_budget = byte_budget.saturating_sub(left_budget);
    let prefix_end = floor_char_boundary(text, left_budget);
    let suffix_start =
        ceil_char_boundary(text, text.len().saturating_sub(right_budget)).max(prefix_end);
    let removed_tokens = approx_tokens(suffix_start.saturating_sub(prefix_end));
    format!(
        "{}…{removed_tokens} tokens truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
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
    use crate::tools::ToolOutputContent;

    use super::truncate_content;

    #[test]
    fn truncates_text_and_preserves_images() {
        let content = vec![
            ToolOutputContent::InputText {
                text: "abcdefgh".to_owned(),
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,a".to_owned(),
                detail: crate::tools::ImageDetail::High,
            },
            ToolOutputContent::InputText {
                text: "ijklmnop".to_owned(),
            },
        ];
        let output = truncate_content(content, Some(2));
        assert!(matches!(
            &output[0],
            ToolOutputContent::InputText { text }
                if text.contains("original token count: 5") && text.contains("tokens truncated")
        ));
        assert!(matches!(output[1], ToolOutputContent::InputImage { .. }));
    }
}
