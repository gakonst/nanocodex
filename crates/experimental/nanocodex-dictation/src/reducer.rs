use nanocodex_oai_api::realtime::RealtimeTranscriptEntry;

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

    pub(crate) fn recover_tail(&mut self, entries: Vec<RealtimeTranscriptEntry>) -> bool {
        let recovered = entries
            .into_iter()
            .filter(|entry| entry.role == "user")
            .map(|entry| entry.text.trim().to_owned())
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
    use nanocodex_oai_api::realtime::RealtimeTranscriptEntry;

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
        assert!(reducer.recover_tail(vec![
            RealtimeTranscriptEntry {
                role: "user".to_owned(),
                text: "one".to_owned(),
            },
            RealtimeTranscriptEntry {
                role: "user".to_owned(),
                text: "two".to_owned(),
            },
        ]));
        assert_eq!(reducer.committable_text(), "one two");
    }

    #[test]
    fn recovers_only_user_text_from_transport_tail() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.recover_tail(vec![
            RealtimeTranscriptEntry {
                role: "assistant".to_owned(),
                text: "ignored".to_owned(),
            },
            RealtimeTranscriptEntry {
                role: "user".to_owned(),
                text: "recovered".to_owned(),
            },
        ]));
        assert_eq!(reducer.committable_text(), "recovered");
    }

    #[test]
    fn empty_transport_tail_preserves_latest_partial() {
        let mut reducer = TranscriptReducer::default();
        assert!(reducer.push_delta("latest words"));

        assert!(!reducer.recover_tail(Vec::new()));
        assert_eq!(reducer.committable_text(), "latest words");
    }
}
