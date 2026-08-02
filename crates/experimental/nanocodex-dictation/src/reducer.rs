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
        self.partial.clear();
        self.push_finalized(text);
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
    fn close_tail_replaces_streamed_state_without_duplication() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.finish_utterance("one"));
        assert!(reducer.push_delta("two"));
        assert!(reducer.replace_finalized(["one".to_owned(), "two".to_owned()]));
        assert_eq!(reducer.committable_text(), "one two");
    }

    #[test]
    fn replacement_normalizes_empty_text() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.replace_finalized([" ".to_owned(), "recovered".to_owned()]));
        assert_eq!(reducer.committable_text(), "recovered");
    }

    #[test]
    fn empty_transport_tail_preserves_latest_partial() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("latest words"));

        assert!(!reducer.replace_finalized(Vec::new()));
        assert_eq!(reducer.committable_text(), "latest words");
    }
}
