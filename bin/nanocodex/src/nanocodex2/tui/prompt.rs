// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Displayable prompt text paired with model-only image content.

use nanocodex::agent::input::{Prompt, UserInput};
use nanocodex_managed::{PromptContent as ManagedPromptContent, PromptInput as ManagedPromptInput};
use std::{fmt, ops::Range};

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Submission {
    text: String,
    images: Vec<SubmissionImage>,
}

#[derive(Clone, Eq, PartialEq)]
struct SubmissionImage {
    range: Range<usize>,
    data_url: String,
}

impl Submission {
    pub(crate) fn text(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }

    pub(crate) fn multimodal(
        text: String,
        images: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> Self {
        let images = images
            .into_iter()
            .map(|(range, data_url)| SubmissionImage { range, data_url })
            .collect();
        Self { text, images }
    }

    pub(crate) fn join(submissions: Vec<Self>) -> Self {
        let mut text = String::new();
        let mut images = Vec::new();
        for (index, submission) in submissions.into_iter().enumerate() {
            if index > 0 {
                text.push_str("\n\n");
            }
            let offset = text.len();
            text.push_str(&submission.text);
            images.extend(submission.images.into_iter().map(|mut image| {
                image.range.start += offset;
                image.range.end += offset;
                image
            }));
        }
        Self { text, images }
    }

    pub(crate) fn prepend_text(mut self, prefix: String) -> Self {
        if prefix.is_empty() {
            return self;
        }
        let separator = if self.text.is_empty() { "" } else { "\n\n" };
        let offset = prefix.len() + separator.len();
        self.text = format!("{prefix}{separator}{}", self.text);
        for image in &mut self.images {
            image.range.start += offset;
            image.range.end += offset;
        }
        self
    }

    pub(crate) fn display_text(&self) -> &str {
        &self.text
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    pub(crate) fn agent_prompt(&self) -> Prompt {
        let mut content = Vec::new();
        let mut cursor = 0;
        for image in &self.images {
            if cursor < image.range.start {
                content.push(UserInput::Text {
                    text: self.text[cursor..image.range.start].to_owned(),
                });
            }
            content.push(UserInput::Image {
                image_url: image.data_url.clone(),
                detail: None,
            });
            cursor = image.range.end;
        }
        if cursor < self.text.len() {
            content.push(UserInput::Text {
                text: self.text[cursor..].to_owned(),
            });
        }
        Prompt::content(content)
    }

    pub(crate) fn managed_prompt(&self) -> ManagedPromptInput {
        let mut content = Vec::new();
        let mut cursor = 0;
        for image in &self.images {
            if cursor < image.range.start {
                content.push(ManagedPromptContent::Text {
                    text: self.text[cursor..image.range.start].to_owned(),
                });
            }
            content.push(ManagedPromptContent::Image {
                image_url: image.data_url.clone(),
                detail: None,
            });
            cursor = image.range.end;
        }
        if cursor < self.text.len() {
            content.push(ManagedPromptContent::Text {
                text: self.text[cursor..].to_owned(),
            });
        }
        ManagedPromptInput::Content(content)
    }
}

impl fmt::Debug for Submission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Submission")
            .field("text", &self.text)
            .field("images", &self.images.len())
            .finish()
    }
}

impl From<String> for Submission {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::Submission;
    use nanocodex::agent::input::{PromptInput, UserInput};
    use nanocodex_managed::{
        PromptContent as ManagedPromptContent, PromptInput as ManagedPromptInput,
    };

    #[test]
    fn multimodal_prompt_replaces_markers_with_ordered_images() {
        let submission = Submission::multimodal(
            "before [Image #1] after".to_owned(),
            [(7..17, "data:image/png;base64,a".to_owned())],
        );
        let prompt = submission.agent_prompt();
        let PromptInput::Content(content) = prompt.instruction else {
            panic!("multimodal submissions should use content input");
        };

        assert!(matches!(&content[0], UserInput::Text { text } if text == "before "));
        assert!(
            matches!(&content[1], UserInput::Image { image_url, .. } if image_url.ends_with(",a"))
        );
        assert!(matches!(&content[2], UserInput::Text { text } if text == " after"));
    }

    #[test]
    fn managed_prompt_preserves_multimodal_order_for_exact_steering() {
        let submission = Submission::multimodal(
            "before [Image #1] after".to_owned(),
            [(7..17, "data:image/png;base64,a".to_owned())],
        );
        let ManagedPromptInput::Content(content) = submission.managed_prompt() else {
            panic!("multimodal submissions should use managed content input");
        };

        assert!(matches!(&content[0], ManagedPromptContent::Text { text } if text == "before "));
        assert!(
            matches!(&content[1], ManagedPromptContent::Image { image_url, .. } if image_url.ends_with(",a"))
        );
        assert!(matches!(&content[2], ManagedPromptContent::Text { text } if text == " after"));
    }
}
