use crate::DictationTranscript;

#[derive(Default)]
pub(crate) struct TranscriptReducer {
    finalized: Vec<String>,
    partial: String,
}

impl TranscriptReducer {
    pub(crate) fn push_delta(&mut self, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        self.partial.push_str(delta);
        true
    }

    pub(crate) fn finish_utterance(&mut self, text: &str) -> bool {
        let before = self.transcript();
        let visible = self.committable_text();
        let partial = std::mem::take(&mut self.partial);
        if partial.trim().is_empty() || transcripts_reconcile(&partial, text) {
            self.push_finalized(text);
        } else if transcript_extends(&visible, text) {
            self.finalized.clear();
            self.push_finalized(text);
        } else {
            // A completed input event refines matching streamed partial text.
            // Other conversation items leave the user-visible dictation in
            // place.
            self.push_finalized(&partial);
        }
        self.transcript() != before
    }

    pub(crate) fn replace_finalized(&mut self, entries: impl IntoIterator<Item = String>) -> bool {
        let recovered = entries
            .into_iter()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        if recovered.is_empty() {
            return false;
        }
        let before = self.transcript();
        let visible = self.committable_text();
        let replacement = join_text(recovered.iter().map(String::as_str));
        if !visible.is_empty() && !transcript_extends(&visible, &replacement) {
            return false;
        }
        self.finalized = recovered;
        self.partial.clear();
        self.transcript() != before
    }

    pub(crate) fn transcript(&self) -> DictationTranscript {
        DictationTranscript {
            stable: join_text(self.finalized.iter().map(String::as_str)),
            unstable: self.partial.trim().to_owned(),
        }
    }

    pub(crate) fn committable_text(&self) -> String {
        let transcript = self.transcript();
        join_text([transcript.stable.as_str(), transcript.unstable.as_str()])
    }

    fn push_finalized(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty()
            || self
                .finalized
                .last()
                .is_some_and(|previous| previous.trim() == text)
        {
            return;
        }
        self.finalized.push(text.to_owned());
    }
}

fn join_text<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut output = String::new();
    for part in parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(part);
    }
    output
}

fn transcripts_reconcile(current: &str, replacement: &str) -> bool {
    let current = canonical_transcript(current);
    let replacement = canonical_transcript(replacement);
    !current.is_empty()
        && !replacement.is_empty()
        && (current.starts_with(&replacement) || replacement.starts_with(&current))
}

fn transcript_extends(prefix: &str, candidate: &str) -> bool {
    let prefix = canonical_transcript(prefix);
    let candidate = canonical_transcript(candidate);
    !prefix.is_empty() && candidate.starts_with(&prefix)
}

fn canonical_transcript(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::TranscriptReducer;

    #[test]
    fn appends_deltas_and_replaces_partial_with_completed_turn() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("hello"));
        assert!(reducer.push_delta(" world"));
        assert_eq!(reducer.transcript().unstable, "hello world");
        assert_eq!(reducer.committable_text(), "hello world");

        assert!(reducer.finish_utterance("Hello world."));
        assert_eq!(reducer.transcript().stable, "Hello world.");
        assert!(reducer.transcript().unstable.is_empty());
        assert_eq!(reducer.committable_text(), "Hello world.");
    }

    #[test]
    fn close_tail_reconciles_streamed_state_once() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("one"));
        assert!(reducer.push_delta("two"));
        assert!(reducer.replace_finalized(["one".to_owned(), "two".to_owned()]));
        assert_eq!(reducer.committable_text(), "one two");
    }

    #[test]
    fn replacement_normalizes_blank_text() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.replace_finalized([" ".to_owned(), "recovered".to_owned()]));
        assert_eq!(reducer.committable_text(), "recovered");
    }

    #[test]
    fn transport_tail_preserves_latest_partial() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("latest words"));

        assert!(!reducer.replace_finalized(Vec::new()));
        assert_eq!(reducer.committable_text(), "latest words");
    }

    #[test]
    fn completion_reconciliation_preserves_visible_dictation() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("please keep my dictated text"));

        assert!(reducer.finish_utterance("cargo test -p nanocodex-bin"));
        assert_eq!(reducer.committable_text(), "please keep my dictated text");
        assert!(reducer.transcript().unstable.is_empty());
    }

    #[test]
    fn transport_tail_reconciliation_preserves_visible_dictation() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("please keep my dictated text"));

        assert!(!reducer.replace_finalized(["cargo test -p nanocodex-bin".to_owned()]));
        assert_eq!(reducer.committable_text(), "please keep my dictated text");
    }

    #[test]
    fn older_transport_tail_preserves_latest_partial() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("stable words"));
        assert!(reducer.push_delta("latest partial"));

        assert!(!reducer.replace_finalized(["stable words".to_owned()]));
        assert_eq!(reducer.committable_text(), "stable words latest partial");
    }

    #[test]
    fn tail_recovery_preserves_complete_stable_text_after_long_gaps() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("first long section"));
        assert!(reducer.finish_utterance("second section after a gap"));

        assert!(!reducer.replace_finalized(["first long section".to_owned()]));
        assert_eq!(
            reducer.committable_text(),
            "first long section second section after a gap"
        );
    }

    #[test]
    fn cumulative_completion_preserves_one_visible_copy() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("something I noticed"));
        assert!(reducer.push_delta("right now"));

        assert!(reducer.finish_utterance("Something I noticed right now."));
        assert_eq!(reducer.committable_text(), "Something I noticed right now.");
        assert_eq!(
            reducer.transcript().stable,
            "Something I noticed right now."
        );
        assert!(reducer.transcript().unstable.is_empty());
    }
}
