//! Input generations own speech. Keep final answers until captions confirm delivery.
use crate::{VoiceEvent, VoiceFailure};
use nanocodex::oai::realtime::RealtimeSession;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
};
use tokio::sync::mpsc;

const MAX_PENDING: usize = 16;
const MAX_SPEECH_BYTES: usize = 990 * 4;

pub(crate) struct SpeechDelivery {
    input_generation: u64,
    owner: Option<(u64, u64)>,
    voice_input: bool,
    pending: VecDeque<String>,
    input_caption: Option<u64>,
    output_caption: Option<(u64, bool)>,
    completed: VecDeque<u64>,
    last_voice_input: Option<u64>,
    events: mpsc::UnboundedSender<VoiceEvent>,
}

impl SpeechDelivery {
    pub(crate) fn new(events: mpsc::UnboundedSender<VoiceEvent>) -> Self {
        Self {
            input_generation: 0,
            owner: None,
            voice_input: true,
            pending: VecDeque::new(),
            input_caption: None,
            output_caption: None,
            completed: VecDeque::new(),
            last_voice_input: None,
            events,
        }
    }

    pub(crate) fn delegate(&mut self, generation: u64) {
        if self.voice_input {
            self.owner = Some((generation, self.input_generation));
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.input_generation = self.input_generation.wrapping_add(1);
        self.owner = None;
        self.voice_input = false;
        self.restore();
    }

    pub(crate) fn new_voice_input(&mut self) {
        self.invalidate();
        self.voice_input = true;
    }

    pub(crate) fn input_started(&mut self) -> bool {
        if self.input_caption.is_some() {
            return false;
        }
        self.new_voice_input();
        self.input_caption = Some(self.input_generation);
        true
    }

    pub(crate) fn input_done(&mut self, text: &str) -> bool {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for word in text.split_whitespace() {
            word.hash(&mut hash);
        }
        let fingerprint = hash.finish();
        match self.input_caption.take() {
            Some(generation) => {
                if generation == self.input_generation {
                    self.voice_input = !text.trim().is_empty();
                    self.last_voice_input = Some(fingerprint);
                }
                false
            }
            None if !text.trim().is_empty() => {
                if !self.voice_input && self.last_voice_input == Some(fingerprint) {
                    return false;
                }
                self.new_voice_input();
                self.last_voice_input = Some(fingerprint);
                true
            }
            None => false,
        }
    }

    pub(crate) fn output_started(&mut self, text: &str) {
        if !text.trim().is_empty() && self.output_caption.is_none() {
            self.output_caption = Some((self.input_generation, !self.pending.is_empty()));
        }
    }

    pub(crate) fn accepts_caption(&self) -> bool {
        self.voice_input
            && self
                .output_caption
                .is_none_or(|(generation, _)| generation == self.input_generation)
    }

    pub(crate) fn caption_owns_output(&self) -> bool {
        self.voice_input
            && self
                .output_caption
                .is_some_and(|(generation, _)| generation == self.input_generation)
    }

    pub(crate) fn finish_caption(&mut self) {
        self.output_caption = None;
    }

    pub(crate) fn captioned(&mut self, text: &str) {
        // A spontaneous acknowledgement must not consume a queued final answer.
        if self.output_caption == Some((self.input_generation, true))
            && let Some(index) = self
                .pending
                .iter()
                .position(|answer| answer.split_whitespace().eq(text.split_whitespace()))
        {
            self.pending.remove(index);
        }
    }

    fn prepare(&mut self, generation: u64, output: String) -> Option<String> {
        if self.completed.contains(&generation) {
            return None;
        }
        if self.completed.len() == MAX_PENDING {
            self.completed.pop_front();
        }
        self.completed.push_back(generation);
        let can_speak = self.voice_input && self.owner == Some((generation, self.input_generation));
        if self.owner.is_some_and(|(owner, _)| owner == generation) {
            self.owner = None;
        }
        let text = output
            .trim()
            .strip_prefix("[FINAL]")
            .unwrap_or(output.trim())
            .trim()
            .to_owned();
        if text.is_empty() {
            return None;
        }
        if !can_speak || text.len() > MAX_SPEECH_BYTES {
            let _ = self
                .events
                .send(VoiceEvent::UndeliveredAnswer { text: output });
            return None;
        }
        if self.pending.len() == MAX_PENDING
            && let Some(text) = self.pending.pop_front()
        {
            let _ = self.events.send(VoiceEvent::UndeliveredAnswer { text });
        }
        self.pending.push_back(text.clone());
        Some(text)
    }

    pub(crate) async fn complete(
        &mut self,
        session: &RealtimeSession,
        generation: u64,
        output: String,
    ) -> Result<(), VoiceFailure> {
        if let Some(text) = self.prepare(generation, output) {
            session.append_speech(text).await?;
        }
        Ok(())
    }

    fn restore(&mut self) {
        for text in self.pending.drain(..) {
            let _ = self.events.send(VoiceEvent::UndeliveredAnswer { text });
        }
    }
}

impl Drop for SpeechDelivery {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_done_after_typing_cannot_reclaim_speech() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut delivery = SpeechDelivery::new(tx);
        assert!(delivery.input_done("voice question"));
        delivery.invalidate();
        assert!(!delivery.input_done("voice question"));
        assert!(!delivery.accepts_caption());
        assert!(delivery.input_done("new question"));
        assert!(delivery.accepts_caption());
    }

    #[test]
    fn interruption_restores_unconfirmed_answer_and_rejects_old_completion() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut delivery = SpeechDelivery::new(tx);
        delivery.delegate(1);
        assert_eq!(
            delivery.prepare(1, "answer one".into()).as_deref(),
            Some("answer one")
        );
        delivery.new_voice_input();
        assert!(
            matches!(rx.try_recv(), Ok(VoiceEvent::UndeliveredAnswer { text }) if text == "answer one")
        );
        assert!(delivery.prepare(3, "late answer".into()).is_none());
        assert!(
            matches!(rx.try_recv(), Ok(VoiceEvent::UndeliveredAnswer { text }) if text == "late answer")
        );
        delivery.delegate(2);
        assert_eq!(
            delivery.prepare(2, "new answer".into()).as_deref(),
            Some("new answer")
        );
    }

    #[test]
    fn acknowledgement_does_not_lose_answer_and_confirmed_caption_is_not_restored() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut delivery = SpeechDelivery::new(tx);
        delivery.delegate(1);
        delivery.prepare(1, "actual answer".into());
        delivery.captioned("Let me check");
        assert_eq!(delivery.pending.len(), 1);
        delivery.output_started("actual");
        delivery.captioned("actual answer");
        drop(delivery);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn stale_caption_and_done_only_caption_cannot_acknowledge_new_answer() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut delivery = SpeechDelivery::new(tx);
        delivery.output_started("old");
        assert!(delivery.input_started());
        assert!(!delivery.input_started());
        delivery.input_done("new question");
        delivery.delegate(1);
        delivery.prepare(1, "new answer".into());
        assert!(!delivery.accepts_caption());
        delivery.captioned("new answer");
        assert_eq!(delivery.pending.len(), 1);
        delivery.finish_caption();
        delivery.captioned("new answer");
        assert_eq!(delivery.pending.len(), 1);
        delivery.output_started("new");
        delivery.captioned("new answer");
        assert!(delivery.pending.is_empty());
    }

    #[test]
    fn oversized_answer_remains_whole_and_repeated_completion_cannot_speak_twice() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut delivery = SpeechDelivery::new(tx);
        delivery.delegate(1);
        let long = "x".repeat(MAX_SPEECH_BYTES + 1);
        assert!(delivery.prepare(1, long.clone()).is_none());
        assert!(
            matches!(rx.try_recv(), Ok(VoiceEvent::UndeliveredAnswer { text }) if text == long)
        );
        assert!(delivery.prepare(1, "duplicate".into()).is_none());
        assert!(rx.try_recv().is_err());
    }
}
