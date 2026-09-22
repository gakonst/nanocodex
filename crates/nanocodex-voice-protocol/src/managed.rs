//! One managed voice protocol boundary for WASM and native bindings.
use serde_json::{Value, json};

use crate::{
    BrowserVoiceEffects, BrowserVoiceProtocol, VoiceHistoryEntry, build_browser_startup_context,
    realtime_delegation, realtime_tail_delegation,
};

pub struct ManagedVoiceProtocol {
    protocol: BrowserVoiceProtocol,
    session_id: String,
}

impl std::ops::Deref for ManagedVoiceProtocol {
    type Target = BrowserVoiceProtocol;
    fn deref(&self) -> &Self::Target {
        &self.protocol
    }
}
impl std::ops::DerefMut for ManagedVoiceProtocol {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.protocol
    }
}

impl ManagedVoiceProtocol {
    pub fn new(voice: &str) -> Result<Self, String> {
        Ok(Self {
            protocol: BrowserVoiceProtocol::new(voice)?,
            session_id: String::new(),
        })
    }

    pub fn bind_session(&mut self, session_id: &str) {
        if self.session_id != session_id {
            self.session_id = session_id.to_owned();
        }
    }

    /// Match Codex's provider-requested handoffs. Completed speech and partial
    /// transcripts do not themselves admit model work or trigger memory reads.
    pub fn requires_agent_admission(&self, payload: &str) -> bool {
        crate::realtime_message_requires_agent_admission(payload)
    }

    /// Explicit speech can play independently of a coding-agent handoff.
    pub fn append_speech(&mut self, text: &str) -> Result<BrowserVoiceEffects, String> {
        let mut effects = self.protocol.append_speech(text)?;
        effects.playback_enabled = Some(true);
        Ok(effects)
    }

    /// Managed events do not inject background memory into voice context.
    pub fn managed_event(&mut self, _envelope: &Value) -> BrowserVoiceEffects {
        BrowserVoiceEffects::default()
    }

    /// JSON is just the binding ABI; all protocol decisions remain in this crate.
    pub fn dispatch(&mut self, command: &Value) -> Result<Value, String> {
        let effects = match command["op"].as_str().unwrap_or_default() {
            "configure" => {
                if !self.session_id.is_empty() {
                    return Err("voice settings require a new call".to_owned());
                }
                let settings = serde_json::from_value(command["settings"].clone())
                    .map_err(|error| format!("invalid voice settings: {error}"))?;
                self.protocol.configure(settings)?;
                return serde_json::to_value(self.protocol.settings())
                    .map_err(|error| error.to_string());
            }
            "settings" => {
                return serde_json::to_value(self.protocol.settings())
                    .map_err(|error| error.to_string());
            }
            "session" => {
                return self.protocol.settings().chatgpt_session(
                    command["instructions"]
                        .as_str()
                        .unwrap_or(&crate::chatgpt_realtime_instructions("there")),
                );
            }
            "speech" => self.append_speech(command["text"].as_str().unwrap_or_default())?,
            "append_context" => self
                .protocol
                .append_context(command["text"].as_str().unwrap_or_default())?,
            "text" => self.protocol.append_text(
                serde_json::from_value(command["role"].clone())
                    .map_err(|_| "invalid voice text role")?,
                command["text"].as_str().unwrap_or_default(),
            )?,
            "catalog" => return Ok(json!(crate::CHATGPT_REALTIME_VOICES)),
            "bind" => {
                self.bind_session(command["session_id"].as_str().unwrap_or_default());
                return Ok(Value::Null);
            }
            "realtime" => {
                let update = self.realtime_message(&command["event"].to_string());
                return Ok(
                    json!({ "effects": update.effects, "prefetch": update.prefetch, "delegation": update.delegation.map(|delegation|
                    json!({ "id": delegation.id, "formatted_input": format_delegation(&delegation) })) }),
                );
            }
            "typed_input" => self.note_typed_input(),
            "agent" => self.agent_event(&command["event"].to_string()),
            "managed" => self.managed_event(&command["envelope"]),
            "context" => self
                .protocol
                .context(command["text"].as_str().unwrap_or_default()),
            "flush" => self.flush(command["final"].as_bool().unwrap_or_default()),
            "opened" => self.sideband_opened(),
            "closed" => self
                .protocol
                .sideband_closed(command["connected_ms"].as_u64().unwrap_or_default()),
            "ack" => {
                self.protocol
                    .frames_sent(command["count"].as_u64().unwrap_or_default().min(128) as usize);
                return Ok(Value::Null);
            }
            "tail" => {
                return Ok(json!(realtime_tail_delegation(
                    &self.protocol.take_transcript_tail()
                )));
            }
            "close" => self.protocol.close_effects(),
            "instructions" => {
                let mut text = crate::chatgpt_realtime_instructions("there");
                if let Some(context) = managed_startup_context(&command["context"]) {
                    text.push_str("\n\n");
                    text.push_str(&context);
                }
                return Ok(json!(self.protocol.settings().instructions(&text)));
            }
            "startup_context" => {
                let frames = managed_startup_context(&command["context"])
                    .map(|text| crate::browser::session_context_frames(&text, "commentary"))
                    .unwrap_or_default();
                return Ok(json!(frames));
            }
            "delegation" => {
                let transcript = command["transcript"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        crate::TranscriptEntry::new(
                            item["speaker"].as_str().unwrap_or_default(),
                            item["text"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>();
                return Ok(if command["tail"].as_bool() == Some(true) {
                    json!(realtime_tail_delegation(&transcript))
                } else {
                    json!(realtime_delegation(
                        command["input"].as_str().unwrap_or_default(),
                        &transcript
                    ))
                });
            }
            _ => return Err("unknown voice protocol operation".to_owned()),
        };
        serde_json::to_value(effects).map_err(|error| error.to_string())
    }
}

pub fn format_delegation(delegation: &crate::browser::BrowserVoiceDelegation) -> String {
    let input = realtime_delegation(&delegation.input, &delegation.transcript);
    if delegation.bootstrap {
        input.replacen(
            "<realtime_delegation>",
            "<realtime_delegation>\n  <source>voice_bootstrap</source>",
            1,
        )
    } else {
        input
    }
}

pub fn managed_startup_context(context: &Value) -> Option<String> {
    let history = context["history"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let role = entry["role"].as_str()?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let text = entry["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            Some(VoiceHistoryEntry::new(role, text))
        })
        .collect::<Vec<_>>();
    let history_context = build_browser_startup_context(
        &history,
        context["workspace"].as_str().unwrap_or_default(),
        &[],
    );
    history_context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_personalization_is_ignored() {
        let context = json!({
            "history": [{"role":"developer","content":[{"text":"private host state"}]}],
            "prepared_personalization": "Prefers short answers."
        });
        assert!(managed_startup_context(&context).is_none());
        let mut voice = ManagedVoiceProtocol::new("cove").unwrap();
        assert_eq!(
            voice
                .dispatch(&json!({"op":"startup_context","context":context}))
                .unwrap(),
            json!([])
        );
    }

    #[test]
    fn native_startup_context_uses_codex_wire_chunks_without_losing_selected_context() {
        let history = (0..6)
            .map(|index| json!({
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": [{"text": format!("FIRST {index} {} LAST {index}", "Ελληνικά 🦊 ".repeat(40))}]
            }))
            .collect::<Vec<_>>();
        let context = json!({"history": history});
        let expected = managed_startup_context(&context).unwrap();
        assert!(expected.len() > 2_000);
        let mut voice = ManagedVoiceProtocol::new("cove").unwrap();
        let result = voice
            .dispatch(&json!({"op":"startup_context","context":context}))
            .unwrap();
        let frames = result.as_array().unwrap();
        assert!(frames.len() > 1);
        let mut reconstructed = String::new();
        for frame in frames {
            assert_eq!(frame["type"], "session.context.append");
            assert_eq!(frame["channel"], "commentary");
            let text = frame["content"][0]["text"].as_str().unwrap();
            assert!(text.len() <= 500);
            reconstructed.push_str(text);
        }
        assert_eq!(reconstructed, expected);
        assert_eq!(
            voice
                .dispatch(&json!({"op":"startup_context","context":null}))
                .unwrap(),
            json!([])
        );
    }

    fn voice() -> ManagedVoiceProtocol {
        let mut voice = ManagedVoiceProtocol::new("cove").unwrap();
        voice.bind_session("call-1");
        voice
    }
    fn utterance(text: &str) -> String {
        json!({"type":"turn.done","turn":{"role":"user","transcript":text}}).to_string()
    }
    fn delegation(id: &str, text: &str) -> String {
        json!({"type":"delegation.created","item":{"type":"delegation","target":"client","id":id,"content":[{"type":"input_text","text":text}]}}).to_string()
    }
    #[test]
    fn conversational_speech_never_forces_retrieval_or_waits_for_agent_output() {
        let mut voice = voice();
        assert_eq!(voice.sideband_opened().playback_enabled, Some(true));
        for text in [
            "Hi, say hello briefly",
            "What is two plus two?",
            "When is Elena's birthday?",
        ] {
            let partial = json!({"type":"input_transcript.added","item":{"text":text}}).to_string();
            for event in [partial, utterance(text)] {
                assert!(!voice.requires_agent_admission(&event));
                let update = voice.realtime_message(&event);
                assert!(update.delegation.is_none());
                assert!(update.prefetch.is_none());
            }
        }
        let reply = voice.realtime_message(
            r#"{"type":"output_transcript.added","item":{"text":"Let me check."}}"#,
        );
        assert_eq!(reply.effects.transcripts.len(), 1);
        assert_eq!(voice.sideband_opened().playback_enabled, Some(true));
        let speech = voice.append_speech("Voice is connected.").unwrap();
        assert_eq!(speech.playback_enabled, Some(true));
        assert_eq!(voice.sideband_opened().frames, speech.frames);
    }
    #[test]
    fn provider_handoff_preserves_lookup_and_transcript_without_synthetic_bootstrap() {
        let mut voice = voice();
        voice.realtime_message(&utterance("When is Elena's birthday?"));
        let event = delegation("provider-1", "Look up the saved birthday; do not guess.");
        assert!(voice.requires_agent_admission(&event));
        let delegated = voice.realtime_message(&event).delegation.unwrap();
        assert!(!delegated.bootstrap);
        assert_eq!(delegated.input, "Look up the saved birthday; do not guess.");
        let formatted = format_delegation(&delegated);
        assert!(formatted.contains("When is Elena's birthday?"));
        assert!(!formatted.contains("voice_bootstrap"));
        assert!(voice.realtime_message(&event).delegation.is_none());
        let output = voice.agent_event(
            r#"{"type":"assistant.message","payload":{"text":"No saved birthday was found."}}"#,
        );
        assert!(output.frames[0].contains("provider-1"));
        assert_eq!(voice.sideband_opened().frames, output.frames);
        voice.frames_sent(output.frames.len());
        assert!(voice.sideband_opened().frames.is_empty());
        voice.realtime_message(&utterance("And what should I get her?"));
        assert!(
            voice
                .realtime_message(&delegation("provider-2", "Suggest a present"))
                .delegation
                .is_some()
        );
    }
    #[test]
    fn failed_run_reports_error_with_or_without_provider_handoff() {
        for handoff in [false, true] {
            for failure in [Some("The subscription request failed."), None] {
                let mut voice = voice();
                if handoff {
                    assert!(
                        voice
                            .realtime_message(&delegation("lookup", "Check this request"))
                            .delegation
                            .is_some()
                    );
                }
                let _ = voice.agent_event(r#"{"type":"run.started"}"#);
                if let Some(failure) = failure {
                    let _ = voice.agent_event(
                        &json!({"type":"run.error","payload":{"text":failure}}).to_string(),
                    );
                }
                let failed = voice.agent_event(r#"{"type":"run.failed"}"#);
                assert_eq!(failed.frames.len(), 1);
                let frame: Value = serde_json::from_str(&failed.frames[0]).unwrap();
                assert_eq!(
                    frame["type"],
                    if handoff {
                        "delegation.context.append"
                    } else {
                        "session.context.append"
                    }
                );
                assert_eq!(
                    frame["content"][0]["text"],
                    failure.unwrap_or("The coding agent failed.")
                );
                assert_eq!(voice.sideband_opened().frames, failed.frames);
                voice.frames_sent(failed.frames.len());
                assert!(voice.sideband_opened().frames.is_empty());
                assert!(
                    voice
                        .agent_event(r#"{"type":"run.failed"}"#)
                        .frames
                        .is_empty()
                );
            }
        }
    }
    #[test]
    fn durable_admission_failure_after_prior_output_finishes_the_handoff_once() {
        let mut voice = voice();
        voice.realtime_message(&delegation("prior", "First lookup"));
        let prior =
            voice.agent_event(r#"{"type":"assistant.message","payload":{"text":"First answer."}}"#);
        voice.frames_sent(prior.frames.len());
        let _ = voice.agent_event(r#"{"type":"run.completed"}"#);
        voice.realtime_message(&delegation("next", "Second lookup"));
        assert!(
            voice
                .agent_event(r#"{"type":"turn_retryable","id":"second"}"#)
                .frames
                .is_empty()
        );
        let failure = r#"{"type":"turn_failed","id":"second","error":"private backend error must not be spoken"}"#;
        let failed = voice.agent_event(failure);
        assert_eq!(failed.frames.len(), 1);
        let frame: Value = serde_json::from_str(&failed.frames[0]).unwrap();
        assert_eq!(frame["type"], "delegation.context.append");
        assert_eq!(frame["delegation_item_id"], "next");
        assert_eq!(
            frame["content"][0]["text"],
            "I couldn't complete that request. Please try again."
        );
        assert_eq!(voice.sideband_opened().frames, failed.frames);
        voice.frames_sent(failed.frames.len());
        assert!(voice.agent_event(failure).frames.is_empty());
        assert!(voice.sideband_opened().frames.is_empty());
    }

    #[test]
    fn provider_handoff_before_final_transcript_is_not_replaced_or_repeated() {
        let mut voice = voice();
        voice.realtime_message(
            r#"{"type":"input_transcript.added","item":{"text":"Tell me about Elena"}}"#,
        );
        let event = delegation("first", "Search personal memory");
        let first = voice.realtime_message(&event).delegation.unwrap();
        assert_eq!(first.input, "Search personal memory");
        assert!(!first.bootstrap);
        assert!(
            voice
                .realtime_message(&utterance("Tell me about Elena"))
                .delegation
                .is_none()
        );
        assert!(voice.realtime_message(&event).delegation.is_none());
        assert!(
            voice
                .realtime_message(&delegation("followup", "Tell me again"))
                .delegation
                .is_some()
        );
    }
    #[test]
    fn legacy_memory_events_are_ignored() {
        let mut voice = voice();
        let event = json!({"cursor":"1","event":{"type":"managed.voice.context","payload":{
            "voice_session_id":"call-1", "result":{"operation":"put","memory":{"key":{"id":5,"version":2},"content":"old memory"}}
        }}});
        assert!(voice.managed_event(&event).frames.is_empty());
    }
}
