//! Client-managed final-answer delivery shared by the browser/WASM bindings.
use crate::BrowserVoiceEffects;
use serde_json::Value;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
};

#[derive(Default)]
pub(crate) struct BrowserSpeechDelivery {
    generation: u64,
    voice_input: bool,
    input_caption: Option<u64>,
    output_caption: Option<(u64, bool)>,
    last_input: Option<u64>,
    owner: Option<u64>,
    candidate: Option<String>,
    pending: VecDeque<(u64, String)>,
    suppressed: bool,
}

impl BrowserSpeechDelivery {
    pub(crate) fn new() -> Self {
        Self {
            voice_input: true,
            ..Self::default()
        }
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn accepts_caption(&self) -> bool {
        self.voice_input
            && self
                .output_caption
                .is_none_or(|(generation, _)| generation == self.generation)
    }

    pub(crate) fn allow_explicit_speech(&mut self) {
        self.voice_input = true;
        self.suppressed = false;
        self.output_caption = None;
    }

    pub(crate) fn playback_enabled(&self) -> bool {
        !self.suppressed
    }

    pub(crate) fn invalidate(&mut self) -> BrowserVoiceEffects {
        self.generation = self.generation.wrapping_add(1);
        self.voice_input = false;
        self.suppressed = true;
        BrowserVoiceEffects {
            input_generation: Some(self.generation),
            playback_enabled: Some(false),
            undelivered_answers: self.pending.drain(..).map(|(_, text)| text).collect(),
            ..BrowserVoiceEffects::default()
        }
    }

    pub(crate) fn close(&mut self) -> BrowserVoiceEffects {
        self.owner = None;
        self.candidate = None;
        self.invalidate()
    }

    pub(crate) fn realtime(&mut self, event: &Value) -> BrowserVoiceEffects {
        let mut effects = BrowserVoiceEffects::default();
        match event["type"].as_str().unwrap_or_default() {
            "input_transcript.added" if self.input_caption.is_none() => {
                effects = self.invalidate();
                self.voice_input = true;
                self.input_caption = Some(self.generation);
            }
            "output_transcript.added" => {
                if event["item"]["text"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
                {
                    self.output_caption
                        .get_or_insert((self.generation, !self.pending.is_empty()));
                    if self.voice_input
                        && self
                            .output_caption
                            .is_some_and(|(generation, _)| generation == self.generation)
                    {
                        self.suppressed = false;
                        effects.playback_enabled = Some(true);
                    }
                }
            }
            "turn.done" => {
                let text = event["turn"]["transcript"].as_str().unwrap_or_default();
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                for word in text.split_whitespace() {
                    word.hash(&mut hash);
                }
                let fingerprint = Some(hash.finish());
                match event["turn"]["role"].as_str() {
                    Some("user") => match self.input_caption.take() {
                        Some(generation) if generation == self.generation => {
                            self.voice_input = !text.trim().is_empty();
                            self.last_input = fingerprint;
                            if !self.voice_input {
                                self.suppressed = false;
                                effects.playback_enabled = Some(true);
                            }
                        }
                        None if !text.trim().is_empty()
                            && (self.voice_input || fingerprint != self.last_input) =>
                        {
                            effects = self.invalidate();
                            self.voice_input = true;
                            self.last_input = fingerprint;
                        }
                        _ => {}
                    },
                    Some("assistant") => {
                        if self.output_caption == Some((self.generation, true))
                            && self.voice_input
                            && let Some(index) =
                                self.pending.iter().position(|(generation, answer)| {
                                    *generation == self.generation
                                        && answer.split_whitespace().eq(text.split_whitespace())
                                })
                        {
                            self.pending.remove(index);
                        }
                        self.output_caption = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        effects
    }

    pub(crate) fn delegate(&mut self) {
        self.owner = self.voice_input.then_some(self.generation);
        self.candidate = None;
    }

    pub(crate) fn agent(&mut self, event: &Value, silent: bool) -> BrowserVoiceEffects {
        let mut effects = BrowserVoiceEffects {
            input_generation: Some(self.generation),
            ..BrowserVoiceEffects::default()
        };
        match event["type"].as_str().unwrap_or_default() {
            "run.started" if self.owner.is_none() => {
                effects = self.invalidate();
            }
            "assistant.message" if self.owner.is_some() => {
                let text = event["payload"]["text"].as_str().unwrap_or_default();
                let phase = event["payload"]["phase"].as_str();
                if !text.trim().is_empty()
                    && phase != Some("commentary")
                    && (phase == Some("final_answer")
                        || !(text.trim_start().starts_with("[ANALYSIS]")
                            || text.trim_start().starts_with("[COMMENTARY]")))
                {
                    self.candidate = Some(text.to_owned());
                }
            }
            "run.completed" => {
                let owner = self.owner.take();
                if let Some(text) = self.candidate.take() {
                    let text = text
                        .trim()
                        .strip_prefix("[FINAL]")
                        .unwrap_or(text.trim())
                        .trim()
                        .to_owned();
                    if text.is_empty() {
                        return effects;
                    }
                    if silent
                        || !self.voice_input
                        || owner != Some(self.generation)
                        || text.len() > 990 * 4
                    {
                        effects.undelivered_answers.push(text);
                    } else {
                        if self.pending.len() == 16
                            && let Some((_, text)) = self.pending.pop_front()
                        {
                            effects.undelivered_answers.push(text);
                        }
                        self.pending.push_back((self.generation, text.clone()));
                        effects.frames = crate::browser::session_context_frames(&text, "speakable")
                            .into_iter()
                            .map(|frame| frame.to_string())
                            .collect();
                        effects.acknowledge_frames = true;
                    }
                }
            }
            "run.failed" | "run.cancelled" | "turn_failed" => {
                let owned = self.owner.take().is_some();
                self.candidate = None;
                effects = self.invalidate();
                if owned && event["type"] != "run.cancelled" {
                    effects
                        .undelivered_answers
                        .push("The coding agent could not complete the request.".into());
                }
            }
            _ => {}
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use crate::BrowserVoiceProtocol;
    use serde_json::json;

    fn voice() -> BrowserVoiceProtocol {
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        voice.enable_client_managed_handoffs();
        voice.realtime_message(&json!({"type":"delegation.created","item":{"type":"delegation","target":"client","id":"one","content":[{"type":"input_text","text":"work"}]}}).to_string());
        voice
    }
    fn message(
        voice: &mut BrowserVoiceProtocol,
        text: &str,
        phase: &str,
    ) -> crate::BrowserVoiceEffects {
        voice.agent_event(
            &json!({"type":"assistant.message","payload":{"text":text,"phase":phase}}).to_string(),
        )
    }

    #[test]
    fn completed_final_is_speakable_and_private_progress_is_never_sent() {
        let mut voice = voice();
        assert!(
            message(&mut voice, "private", "commentary")
                .frames
                .is_empty()
        );
        assert!(voice.flush(true).frames.is_empty());
        assert!(
            message(&mut voice, "[FINAL] done", "final_answer")
                .frames
                .is_empty()
        );
        let completed = voice.agent_event(r#"{"type":"run.completed"}"#);
        assert_eq!(completed.frames.len(), 1);
        let frame: serde_json::Value = serde_json::from_str(&completed.frames[0]).unwrap();
        assert_eq!(frame["channel"], "speakable");
        assert_eq!(frame["content"][0]["text"], "done");
        assert!(
            voice
                .agent_event(r#"{"type":"run.completed"}"#)
                .frames
                .is_empty()
        );
        assert_eq!(voice.close_effects().undelivered_answers, ["done"]);
        assert!(voice.close_effects().undelivered_answers.is_empty());
    }

    #[test]
    fn typed_input_suppresses_pending_speech_and_reconnect_cannot_replay_it() {
        let mut voice = voice();
        message(&mut voice, "done", "final_answer");
        let _ = voice.agent_event(r#"{"type":"run.completed"}"#);
        let typed = voice.note_typed_input();
        assert_eq!(typed.playback_enabled, Some(false));
        assert_eq!(typed.undelivered_answers, ["done"]);
        assert!(voice.sideband_opened().frames.is_empty());
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
    }

    #[test]
    fn new_speech_invalidates_old_completion_and_caption_ack_requires_ownership() {
        let mut voice = voice();
        message(&mut voice, "late", "final_answer");
        let input = voice.realtime_message(
            r#"{"type":"input_transcript.added","item":{"text":"new question"}}"#,
        );
        assert_eq!(input.effects.playback_enabled, Some(false));
        let completed = voice.agent_event(r#"{"type":"run.completed"}"#);
        assert!(completed.frames.is_empty());
        assert_eq!(completed.undelivered_answers, ["late"]);

        let mut voice = self::voice();
        message(&mut voice, "answer", "final_answer");
        let _ = voice.agent_event(r#"{"type":"run.completed"}"#);
        voice.frames_sent(1); // Queue acceptance alone is not proof of delivery.
        voice.realtime_message(
            r#"{"type":"turn.done","turn":{"role":"assistant","transcript":"answer"}}"#,
        );
        assert_eq!(voice.close_effects().undelivered_answers, ["answer"]);

        let mut voice = self::voice();
        message(&mut voice, "answer", "final_answer");
        let _ = voice.agent_event(r#"{"type":"run.completed"}"#);
        voice.realtime_message(r#"{"type":"output_transcript.added","item":{"text":"answer"}}"#);
        voice.realtime_message(
            r#"{"type":"turn.done","turn":{"role":"assistant","transcript":"answer"}}"#,
        );
        assert!(voice.close_effects().undelivered_answers.is_empty());
    }

    #[test]
    fn repeated_input_and_late_captions_cannot_reclaim_typed_input() {
        let mut voice = voice();
        voice.realtime_message(
            r#"{"type":"turn.done","turn":{"role":"user","transcript":"same question"}}"#,
        );
        voice.note_typed_input();
        let repeated = voice.realtime_message(
            r#"{"type":"turn.done","turn":{"role":"user","transcript":" same  question "}}"#,
        );
        assert_ne!(repeated.effects.playback_enabled, Some(true));
        let caption = voice.realtime_message(
            r#"{"type":"output_transcript.added","item":{"text":"obsolete answer"}}"#,
        );
        assert!(caption.effects.transcripts.is_empty());
        let done = voice.realtime_message(
            r#"{"type":"turn.done","turn":{"role":"assistant","transcript":"obsolete answer"}}"#,
        );
        assert!(done.effects.transcripts.is_empty());
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
    }

    #[test]
    fn explicit_speech_can_resume_after_typing_and_survives_reconnect() {
        let mut voice = voice();
        let typed = voice.note_typed_input();
        let speech = voice.append_speech("Read this").unwrap();
        assert_eq!(speech.input_generation, typed.input_generation);
        assert_eq!(speech.playback_enabled, Some(true));
        assert_eq!(voice.sideband_opened().playback_enabled, Some(true));
        let caption = voice
            .realtime_message(r#"{"type":"output_transcript.added","item":{"text":"Read this"}}"#);
        assert_eq!(caption.effects.transcripts.len(), 1);
    }

    #[test]
    fn oversized_finals_and_failed_turns_do_not_leak_as_speech() {
        let mut voice = voice();
        let long = "x".repeat(3961);
        message(&mut voice, &long, "final_answer");
        let completed = voice.agent_event(r#"{"type":"run.completed"}"#);
        assert!(completed.frames.is_empty());
        assert_eq!(completed.undelivered_answers, [long]);
        let mut voice = self::voice();
        message(&mut voice, "unfinished", "final_answer");
        let failed = voice.agent_event(r#"{"type":"run.failed"}"#);
        assert!(failed.frames.is_empty());
        assert_eq!(
            failed.undelivered_answers,
            ["The coding agent could not complete the request."]
        );
    }
}
