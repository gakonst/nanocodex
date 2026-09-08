//! One managed voice protocol boundary for WASM and native bindings.
use serde_json::{Value, json};

use crate::{
    BrowserVoiceEffects, BrowserVoiceProtocol, VoiceHistoryEntry, build_browser_startup_context,
    realtime_delegation, realtime_tail_delegation,
};

pub struct ManagedVoiceProtocol {
    protocol: BrowserVoiceProtocol,
    session_id: String,
    context_cursor: String,
    bootstrap_input: Option<String>,
    bootstrap_complete: bool,
    explicit_playback: bool,
    follow_up: bool,
    awaiting_first_turn_done: bool,
    prefetch_query: String,
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
            context_cursor: "0".to_owned(),
            bootstrap_input: None,
            bootstrap_complete: false,
            explicit_playback: false,
            follow_up: false,
            awaiting_first_turn_done: false,
            prefetch_query: String::new(),
        })
    }

    pub fn bind_session(&mut self, session_id: &str) {
        if self.session_id != session_id {
            self.session_id = session_id.to_owned();
            self.context_cursor = "0".to_owned();
            self.bootstrap_input = None;
            self.bootstrap_complete = false;
            self.explicit_playback = false;
            self.follow_up = false;
            self.awaiting_first_turn_done = false;
            self.prefetch_query.clear();
        }
    }

    /// The first completed utterance owns retrieval even if the provider would
    /// otherwise answer it directly. A later provider delegation adopts the
    /// handoff channel without admitting that same first request twice.
    pub fn realtime_message(&mut self, payload: &str) -> crate::BrowserVoiceUpdate {
        let mut update = self.protocol.realtime_message(payload);
        if self.session_id.is_empty() {
            return update;
        }
        let event: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let utterance = if event["type"] == "turn.done" && event["turn"]["role"] == "user" {
            event["turn"]["transcript"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
        } else {
            None
        };
        // A partial transcript can warm retrieval, but can never admit a turn.
        // The host only reuses these results for an exactly matching final plan.
        if self.bootstrap_input.is_none()
            && utterance.is_none()
            && update.delegation.is_none()
            && let Some(entry) = update
                .effects
                .transcripts
                .iter()
                .rev()
                .find(|entry| entry.speaker == "user")
        {
            let plan = bootstrap_plan(&entry.text);
            let query = plan["query"].as_str().unwrap_or_default();
            if query.len() >= 12 && query != self.prefetch_query {
                self.prefetch_query = query.to_owned();
                update.prefetch = Some(crate::VoicePrefetch {
                    query: query.to_owned(),
                    debounce_ms: 250,
                });
            }
        }
        if !self.bootstrap_complete && !self.explicit_playback {
            update
                .effects
                .transcripts
                .retain(|entry| entry.speaker == "user");
        }
        if self.bootstrap_input.is_some() {
            if utterance.is_some() {
                if self.awaiting_first_turn_done {
                    self.awaiting_first_turn_done = false;
                } else {
                    self.follow_up = true;
                }
            }
            if !self.follow_up {
                update.delegation = None;
            }
            return update;
        }
        let query = utterance.map(str::to_owned).or_else(|| {
            update.delegation.as_ref().map(|delegation| {
                delegation
                    .transcript
                    .iter()
                    .find(|entry| entry.role == "user")
                    .map_or_else(|| delegation.input.clone(), |entry| entry.text.clone())
            })
        });
        if let Some(input) = query {
            self.explicit_playback = false;
            self.awaiting_first_turn_done = utterance.is_none();
            self.bootstrap_input = Some(input.clone());
            if let Some(delegation) = &mut update.delegation {
                delegation.input = input;
                delegation.bootstrap = true;
                delegation.transcript.retain(|entry| entry.role == "user");
            } else {
                update.delegation = Some(crate::browser::BrowserVoiceDelegation {
                    id: "voice-bootstrap".to_owned(),
                    input,
                    transcript: self
                        .protocol
                        .take_transcript_tail()
                        .into_iter()
                        .filter(|entry| entry.role == "user")
                        .collect(),
                    bootstrap: true,
                });
            }
            update.effects.playback_enabled = Some(false);
        }
        update
    }

    pub fn requires_agent_admission(&self, payload: &str) -> bool {
        if crate::realtime_message_requires_agent_admission(payload) {
            return true;
        }
        if self.session_id.is_empty() || self.bootstrap_input.is_some() {
            return false;
        }
        let event: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        event["type"] == "turn.done"
            && event["turn"]["role"] == "user"
            && event["turn"]["transcript"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
    }

    pub fn agent_event(&mut self, payload: &str) -> BrowserVoiceEffects {
        let effects = self.protocol.agent_event(payload);
        self.with_playback(effects)
    }

    pub fn flush(&mut self, final_chunk: bool) -> BrowserVoiceEffects {
        let effects = self.protocol.flush(final_chunk);
        self.with_playback(effects)
    }

    pub fn sideband_opened(&self) -> BrowserVoiceEffects {
        let mut effects = self.protocol.sideband_opened();
        effects.playback_enabled =
            Some(self.session_id.is_empty() || self.bootstrap_complete || self.explicit_playback);
        effects
    }

    /// Explicit speech can play before the first utterance without satisfying
    /// the first-utterance retrieval gate or admitting a coding-agent turn.
    pub fn append_speech(&mut self, text: &str) -> Result<BrowserVoiceEffects, String> {
        let mut effects = self.protocol.append_speech(text)?;
        self.explicit_playback = true;
        effects.playback_enabled = Some(true);
        Ok(effects)
    }

    const fn with_playback(&mut self, mut effects: BrowserVoiceEffects) -> BrowserVoiceEffects {
        if self.bootstrap_input.is_some() && !self.bootstrap_complete && !effects.frames.is_empty()
        {
            self.bootstrap_complete = true;
            effects.playback_enabled = Some(true);
        }
        effects
    }

    /// Ignore stale/replayed context without converting decimal cursors to floats.
    pub fn managed_event(&mut self, envelope: &Value) -> BrowserVoiceEffects {
        let event = &envelope["event"];
        let payload = &event["payload"];
        let cursor = envelope["cursor"].as_str().unwrap_or_default();
        if event["type"] != "managed.voice.context"
            || self.session_id.is_empty()
            || payload["voice_session_id"].as_str() != Some(&self.session_id)
            || cursor.len() > 32
            || cursor.starts_with('0')
            || cursor.is_empty()
            || !cursor.bytes().all(|byte| byte.is_ascii_digit())
            || (cursor.len(), cursor) <= (self.context_cursor.len(), self.context_cursor.as_str())
        {
            return BrowserVoiceEffects::default();
        }
        let Some(text) = memory_update(&payload["result"]) else {
            return BrowserVoiceEffects::default();
        };
        self.context_cursor = cursor.to_owned();
        self.protocol.context(&text)
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

/// Shared typed/voice search policy. The host executes and persists these calls.
pub fn bootstrap_plan(input: &str) -> Value {
    let voice = input.starts_with("<realtime_delegation>\n  <source>voice_bootstrap</source>");
    let text = if voice {
        input
            .split_once("<input>")
            .and_then(|(_, tail)| tail.split_once("</input>"))
            .map_or_else(
                || input.to_owned(),
                |(text, _)| {
                    text.replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&amp;", "&")
                },
            )
    } else {
        input.to_owned()
    };
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = normalized.len().min(512);
    while !normalized.is_char_boundary(end) {
        end -= 1;
    }
    let query = normalized[..end].trim();
    let query = if query.is_empty() {
        "conversation context"
    } else {
        query
    };
    json!({ "voice_bootstrap": voice, "query": query, "calls": [
        { "name": "find_session", "arguments": { "query": query, "limit": 5 } },
        { "name": "memory", "arguments": { "operation": "scan", "query": query, "limit": 5 } },
    ] })
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
    build_browser_startup_context(
        &history,
        context["workspace"].as_str().unwrap_or_default(),
        &[],
    )
}

fn memory_update(result: &Value) -> Option<String> {
    let operation = result["operation"].as_str()?;
    let key = if operation == "put" {
        &result["memory"]["key"]
    } else {
        &result["key"]
    };
    let (id, version) = (key["id"].as_u64()?, key["version"].as_u64()?);
    if id == 0 || version == 0 || id > 9_007_199_254_740_991 || version > 9_007_199_254_740_991 {
        return None;
    }
    let mut data = json!({ "operation": operation, "key": { "id": id, "version": version } });
    if operation == "put" {
        let content = result["memory"]["content"].as_str()?;
        if content.trim().is_empty() || content.len() > 1024 {
            return None;
        }
        data["content"] = json!(content);
    } else if operation != "delete" {
        return None;
    }
    Some(format!(
        "Saved-memory update (background data):\n{}",
        data.to_string()
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn explicit_speech_unlocks_playback_without_skipping_first_utterance_retrieval() {
        let mut voice = ManagedVoiceProtocol::new("cove").unwrap();
        voice.bind_session("call-1");
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
        let speech = voice.append_speech("Voice is connected.").unwrap();
        assert_eq!(speech.playback_enabled, Some(true));
        assert_eq!(voice.sideband_opened().frames, speech.frames);
        assert_eq!(voice.sideband_opened().playback_enabled, Some(true));
        let transcript = voice.realtime_message(
            r#"{"type":"output_transcript.added","item":{"text":"Voice is connected."}}"#,
        );
        assert_eq!(transcript.effects.transcripts.len(), 1);
        let first = voice.realtime_message(r#"{"type":"turn.done","turn":{"role":"user","transcript":"What is my project called?"}}"#);
        assert!(first.delegation.unwrap().bootstrap);
        assert_eq!(first.effects.playback_enabled, Some(false));
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
    }
    #[test]
    fn partial_speech_only_prefetches_and_final_speech_still_owns_admission() {
        let mut voice = voice();
        let partial =
            r#"{"type":"input_transcript.added","item":{"text":"When is Elena's birthday?"}}"#;
        let early = voice.realtime_message(partial);
        assert!(early.delegation.is_none());
        assert_eq!(
            early.prefetch.unwrap(),
            crate::VoicePrefetch {
                query: "When is Elena's birthday?".to_owned(),
                debounce_ms: 250,
            }
        );
        assert!(
            voice
                .realtime_message(r#"{"type":"input_transcript.added","item":{"text":" "}}"#)
                .prefetch
                .is_none()
        );
        let completed = voice.realtime_message(&utterance("When is Elena's birthday?"));
        assert!(completed.delegation.unwrap().bootstrap);
        assert!(completed.prefetch.is_none());
        assert!(voice.realtime_message(partial).prefetch.is_none());
        voice.bind_session("another-call");
        assert!(voice.realtime_message(partial).prefetch.is_some());
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
    fn first_utterance_retrieves_before_playback_and_adopts_provider_handoff_once() {
        let mut voice = voice();
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
        assert!(
            voice
                .realtime_message(r#"{"type":"output_transcript.added","item":{"text":"A guess"}}"#)
                .effects
                .transcripts
                .is_empty()
        );
        let input = utterance("When is Elena's birthday?");
        assert!(voice.requires_agent_admission(&input));
        let first = voice.realtime_message(&input);
        let delegated = first.delegation.unwrap();
        assert!(delegated.bootstrap);
        let plan = bootstrap_plan(&format_delegation(&delegated));
        assert_eq!(plan["query"], "When is Elena's birthday?");
        assert!(!format_delegation(&delegated).contains("A guess"));
        assert!(!voice.requires_agent_admission(&input));
        assert!(
            voice
                .realtime_message(&delegation("provider-1", "Look up the date"))
                .delegation
                .is_none()
        );
        let output = voice.agent_event(
            r#"{"type":"assistant.message","payload":{"text":"The saved date is December 22."}}"#,
        );
        assert_eq!(output.playback_enabled, Some(true));
        assert!(output.frames[0].contains("provider-1"));
        assert_eq!(voice.sideband_opened().frames, output.frames);
        voice.frames_sent(output.frames.len());
        assert!(voice.sideband_opened().frames.is_empty());
        voice.realtime_message(&utterance("And what should I get her?"));
        let next = voice.realtime_message(&delegation("provider-2", "Suggest a present"));
        assert!(!next.delegation.unwrap().bootstrap);
    }
    #[test]
    fn failed_bootstrap_reports_error_without_waiting_for_provider_delegation() {
        for failure in [Some("The subscription request failed."), None] {
            let mut voice = voice();
            let first = voice.realtime_message(&utterance("Check this request"));
            assert!(first.delegation.unwrap().bootstrap);
            assert_eq!(first.effects.playback_enabled, Some(false));
            voice.agent_event(r#"{"type":"run.started"}"#);
            if let Some(failure) = failure {
                voice.agent_event(
                    &json!({"type":"run.error","payload":{"text":failure}}).to_string(),
                );
            }
            let failed = voice.agent_event(r#"{"type":"run.failed"}"#);
            assert_eq!(failed.playback_enabled, Some(true));
            assert_eq!(failed.frames.len(), 1);
            let frame: Value = serde_json::from_str(&failed.frames[0]).unwrap();
            assert_eq!(frame["type"], "session.context.append");
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
    #[test]
    fn delegation_before_final_transcript_bootstraps_once_and_preserves_followups() {
        let mut voice = voice();
        voice.realtime_message(
            r#"{"type":"input_transcript.added","item":{"text":"Tell me about Elena"}}"#,
        );
        let first = voice.realtime_message(&delegation("first", "Search personal memory"));
        assert_eq!(first.delegation.unwrap().input, "Tell me about Elena");
        assert!(
            voice
                .realtime_message(&utterance("Tell me about Elena"))
                .delegation
                .is_none()
        );
        assert!(
            voice
                .realtime_message(&delegation("same-request", "Search personal memory"))
                .delegation
                .is_none()
        );
        voice.realtime_message(&utterance("Tell me about Elena"));
        assert!(
            voice
                .realtime_message(&delegation("followup", "Tell me again"))
                .delegation
                .is_some()
        );
    }
    #[test]
    fn search_plan_uses_exact_spoken_text_and_utf8_bounds() {
        let mut voice = voice();
        let input = "  Elena <birthday> &\n presents ";
        let first = voice
            .realtime_message(&utterance(input))
            .delegation
            .unwrap();
        let plan = bootstrap_plan(&format_delegation(&first));
        assert_eq!(plan["query"], "Elena <birthday> & presents");
        assert_eq!(
            plan["calls"][0]["arguments"]["query"],
            plan["calls"][1]["arguments"]["query"]
        );
        assert_eq!(plan["calls"][1]["arguments"]["operation"], "scan");
        assert_eq!(
            bootstrap_plan(&"😀".repeat(200))["query"]
                .as_str()
                .unwrap()
                .len(),
            512
        );
        assert_eq!(bootstrap_plan("")["query"], "conversation context");
        assert_eq!(bootstrap_plan("Elena")["voice_bootstrap"], false);
    }
    #[test]
    fn memory_updates_are_scoped_bounded_replayable_and_do_not_unlock_playback() {
        let mut voice = voice();
        voice.realtime_message(&utterance("Remember this"));
        let mut event = json!({"cursor":"9007199254740993","event":{"type":"managed.voice.context","payload":{
            "voice_session_id":"wrong-call", "result":{"operation":"put","memory":{"key":{"id":5,"version":2},"content":"Elena <new date>"}}
        }}});
        assert!(voice.managed_event(&event).frames.is_empty());
        event["event"]["payload"]["voice_session_id"] = json!("call-1");
        let update = voice.managed_event(&event);
        assert_eq!(update.frames.len(), 1);
        assert_eq!(update.playback_enabled, None);
        assert!(update.transcripts.is_empty());
        assert!(update.frames[0].contains("u003c"));
        assert_eq!(voice.sideband_opened().playback_enabled, Some(false));
        assert_eq!(voice.sideband_opened().frames, update.frames);
        assert!(voice.managed_event(&event).frames.is_empty());
        event["cursor"] = json!("9007199254740992");
        assert!(voice.managed_event(&event).frames.is_empty());
        event["cursor"] = json!("9007199254740994");
        event["event"]["payload"]["result"] =
            json!({"operation":"delete","key":{"id":5,"version":3}});
        assert_eq!(voice.managed_event(&event).frames.len(), 1);
        voice.frames_sent(2);
        assert!(voice.sideband_opened().frames.is_empty());
        event["cursor"] = json!("9007199254740995");
        event["event"]["payload"]["result"] = json!({"operation":"put","memory":{"key":{"id":5,"version":4},"content":"🦊".repeat(257)}});
        assert!(voice.managed_event(&event).frames.is_empty());
    }
}
