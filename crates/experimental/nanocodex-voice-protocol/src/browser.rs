use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

pub const CHATGPT_REALTIME_VOICE: &str = "cove";
pub const CHATGPT_REALTIME_MODEL: &str = "gpt-live-1-codex";
pub const CHATGPT_REALTIME_VOICES: &[&str] = &[
    "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove",
];

const CURRENT_THREAD_BUDGET: usize = 1_200;
const WORKSPACE_BUDGET: usize = 1_600;
const NOTES_BUDGET: usize = 300;
const TOTAL_BUDGET: usize = 5_300;
const TURN_BUDGET: usize = 300;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const MAX_ACTIVE_TRANSCRIPT_BYTES: usize = 8 * 1024;
const TRUNCATED_TRANSCRIPT_PREFIX: &str = "…";
const REALTIME_OUTPUT_BYTE_LIMIT: usize = 4_000;
const CONTEXT_APPEND_MAX_BYTES: usize = 500;
const RECONNECT_BASE_DELAY_MS: u64 = 200;
const RECONNECT_MAX_DELAY_MS: u64 = 5_000;
const STABLE_CONNECTION_DURATION_MS: u64 = 30_000;
const HANDOFF_STREAM_TRUNCATION_MARKER: &str = "\n…output truncated…\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoiceHistoryEntry {
    pub role: String,
    pub text: String,
}

impl VoiceHistoryEntry {
    #[must_use]
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
        }
    }
}

#[must_use]
pub fn build_browser_startup_context(
    history: &[VoiceHistoryEntry],
    workspace_path: &str,
    workspace_tree: &[String],
) -> Option<String> {
    let current = current_thread(history);
    let workspace = workspace_map(workspace_path, workspace_tree);
    if current.is_none() && workspace.is_none() {
        return None;
    }
    let mut parts = vec![concat!(
        "Startup context from Codex.\n",
        "This is background context about recent work and machine/workspace layout. It may be incomplete or stale. Use it to inform responses, and do not repeat it back unless relevant."
    )
    .to_owned()];
    section(&mut parts, "Current Thread", current, CURRENT_THREAD_BUDGET);
    section(
        &mut parts,
        "Machine / Workspace Map",
        workspace,
        WORKSPACE_BUDGET,
    );
    section(
        &mut parts,
        "Notes",
        Some("Built at realtime startup from the current thread history and a bounded browser workspace scan. This excludes repo memory instructions, AGENTS files, project-doc prompt blends, and memory summaries.".to_owned()),
        NOTES_BUDGET,
    );
    Some(truncate(
        &format!(
            "<startup_context>\n{}\n</startup_context>",
            parts.join("\n\n")
        ),
        TOTAL_BUDGET,
    ))
}

pub fn build_chatgpt_realtime_call(
    sdp: &str,
    voice: &str,
    startup_context: Option<&str>,
) -> Result<String, String> {
    if sdp.trim().is_empty() {
        return Err("browser voice requires an SDP offer".to_owned());
    }
    if !CHATGPT_REALTIME_VOICES.contains(&voice) {
        return Err(format!("unsupported ChatGPT voice: {voice}"));
    }
    let mut instructions = super::chatgpt_realtime_instructions("there");
    if let Some(context) = startup_context.filter(|context| !context.is_empty()) {
        instructions.push_str("\n\n");
        instructions.push_str(context);
    }
    Ok(json!({
        "sdp": sdp,
        "session": {
            "model": CHATGPT_REALTIME_MODEL,
            "instructions": instructions,
            "audio": {
                "output": { "voice": voice },
            },
            "delegation": { "type": "client" },
        },
    })
    .to_string())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BrowserRealtimeCallResult {
    pub call_id: String,
    pub sdp: String,
}

/// Decodes the two provider values returned by Codex's Realtime call endpoint.
pub fn decode_chatgpt_realtime_call(
    response_body: &str,
    location: &str,
) -> Result<BrowserRealtimeCallResult, String> {
    if response_body.trim().is_empty() {
        return Err("Realtime call response contained an empty SDP answer".to_owned());
    }
    let call_id = location
        .split('?')
        .next()
        .unwrap_or(location)
        .rsplit('/')
        .find(|segment| valid_realtime_call_id(segment))
        .ok_or_else(|| format!("Realtime call Location does not contain a call ID: {location}"))?;
    Ok(BrowserRealtimeCallResult {
        call_id: call_id.to_owned(),
        sdp: response_body.to_owned(),
    })
}

pub fn valid_realtime_call_id(value: &str) -> bool {
    if let Some(suffix) = value.strip_prefix("rtc_")
        && !suffix.is_empty()
        && suffix.len() <= 196
        && suffix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character))
    {
        return true;
    }
    value.len() == 36
        && value.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_hexdigit(),
        })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BrowserTranscript {
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BrowserVoiceEffects {
    pub frames: Vec<String>,
    pub transcripts: Vec<BrowserTranscript>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect_after_ms: Option<u64>,
    pub acknowledge_frames: bool,
    pub schedule_flush: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserVoiceDelegation {
    pub input: String,
    pub transcript: Vec<super::TranscriptEntry>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserVoiceUpdate {
    pub effects: BrowserVoiceEffects,
    pub delegation: Option<BrowserVoiceDelegation>,
}

pub struct BrowserVoiceProtocol {
    voice: String,
    transcript: Vec<super::TranscriptEntry>,
    new_input_entry: bool,
    new_output_entry: bool,
    active_delegation: Option<String>,
    output: HandoffStream,
    streamed_this_message: bool,
    output_sent_this_run: bool,
    run_error: Option<String>,
    pending_frames: VecDeque<String>,
    rapid_disconnects: u32,
}

#[must_use]
pub fn realtime_message_requires_agent_admission(payload: &str) -> bool {
    let Ok(event) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    event.get("type").and_then(Value::as_str) == Some("delegation.created")
        && browser_voice_delegation(&event).is_some()
}

impl BrowserVoiceProtocol {
    pub fn new(voice: &str) -> Result<Self, String> {
        if !CHATGPT_REALTIME_VOICES.contains(&voice) {
            return Err(format!("unsupported ChatGPT voice: {voice}"));
        }
        Ok(Self {
            voice: voice.to_owned(),
            transcript: Vec::new(),
            new_input_entry: false,
            new_output_entry: false,
            active_delegation: None,
            output: HandoffStream::default(),
            streamed_this_message: false,
            output_sent_this_run: false,
            run_error: None,
            pending_frames: VecDeque::new(),
            rapid_disconnects: 0,
        })
    }

    #[must_use]
    pub fn voice(&self) -> &str {
        &self.voice
    }

    pub fn realtime_message(&mut self, payload: &str) -> BrowserVoiceUpdate {
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return BrowserVoiceUpdate::default();
        };
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return BrowserVoiceUpdate::default();
        };
        let mut update = BrowserVoiceUpdate::default();
        match kind {
            "error" => {
                let status = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        event
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .or_else(|| event.get("error").map(ToString::to_string))
                    .map_or_else(
                        || "Voice failed".to_owned(),
                        |message| format!("Voice: {message}"),
                    );
                update.effects.terminate = Some(status);
            }
            "session.started" | "session.updated" => {
                update.effects.status = Some(format!("Voice active ({})", self.voice));
            }
            "input_transcript.added" | "output_transcript.added" => {
                let speaker = if kind == "input_transcript.added" {
                    "user"
                } else {
                    "assistant"
                };
                let text = event
                    .pointer("/item/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.is_empty() {
                    let force_new = if speaker == "user" {
                        self.new_input_entry
                    } else {
                        self.new_output_entry
                    };
                    append_transcript(&mut self.transcript, speaker, text, force_new);
                    if speaker == "user" {
                        self.new_input_entry = false;
                        update.effects.status =
                            Some(format!("Voice active ({}) — hearing you…", self.voice));
                    } else {
                        self.new_output_entry = false;
                    }
                }
            }
            "turn.done" => {
                let role = event.pointer("/turn/role").and_then(Value::as_str);
                let text = event
                    .pointer("/turn/transcript")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(role, Some("user" | "assistant")) && !text.is_empty() {
                    let role = role.unwrap_or_default();
                    let force_new = if role == "user" {
                        self.new_input_entry
                    } else {
                        self.new_output_entry
                    };
                    complete_transcript(&mut self.transcript, role, text, force_new);
                    if role == "user" {
                        self.new_input_entry = true;
                    } else {
                        self.new_output_entry = true;
                    }
                    update.effects.transcripts.push(BrowserTranscript {
                        speaker: role.to_owned(),
                        text: text.to_owned(),
                    });
                }
            }
            "delegation.created" => {
                let Some((id, input)) = browser_voice_delegation(&event) else {
                    return update;
                };
                self.active_delegation = Some(id);
                if !self
                    .transcript
                    .iter()
                    .any(|entry| entry.role == "user" && entry.text == input)
                {
                    self.transcript
                        .push(super::TranscriptEntry::new("user", input.clone()));
                }
                update.delegation = Some(BrowserVoiceDelegation {
                    input,
                    transcript: std::mem::take(&mut self.transcript),
                });
                self.new_input_entry = true;
                self.new_output_entry = true;
            }
            _ => {}
        }
        truncate_active_transcript(&mut self.transcript);
        update
    }

    #[must_use]
    pub fn agent_event(&mut self, payload: &str) -> BrowserVoiceEffects {
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return BrowserVoiceEffects::default();
        };
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return BrowserVoiceEffects::default();
        };
        match kind {
            "run.started" => {
                self.streamed_this_message = false;
                self.output_sent_this_run = false;
                self.run_error = None;
                self.output = HandoffStream::default();
                BrowserVoiceEffects::default()
            }
            "assistant.delta" => {
                let text = payload_text(&event);
                if text.is_empty() {
                    return BrowserVoiceEffects::default();
                }
                self.streamed_this_message = true;
                self.output.push_text(text);
                BrowserVoiceEffects {
                    schedule_flush: true,
                    ..BrowserVoiceEffects::default()
                }
            }
            "assistant.message" => {
                let text = payload_text(&event);
                if !text.is_empty() && !self.streamed_this_message {
                    self.output.push_text(text);
                }
                let effects = self.flush(true);
                self.output = HandoffStream::default();
                self.streamed_this_message = false;
                effects
            }
            "run.error" => {
                self.run_error = Some(if payload_text(&event).is_empty() {
                    "The coding agent failed.".to_owned()
                } else {
                    payload_text(&event).to_owned()
                });
                BrowserVoiceEffects::default()
            }
            "run.completed" | "run.failed" => {
                let mut effects = self.flush(true);
                if kind == "run.failed"
                    && self.active_delegation.is_some()
                    && !self.output_sent_this_run
                {
                    let output = self
                        .run_error
                        .clone()
                        .unwrap_or_else(|| "The coding agent failed.".to_owned());
                    self.push_output_frames(&output, &mut effects.frames);
                    effects.acknowledge_frames = true;
                }
                self.output = HandoffStream::default();
                self.active_delegation = None;
                effects
            }
            _ => BrowserVoiceEffects::default(),
        }
    }

    #[must_use]
    pub fn flush(&mut self, final_chunk: bool) -> BrowserVoiceEffects {
        let output = if final_chunk {
            self.output.drain_final_chunk()
        } else {
            self.output.drain_stream_chunk()
        };
        let mut effects = BrowserVoiceEffects::default();
        if let Some(output) = output {
            self.push_output_frames(&output, &mut effects.frames);
            effects.acknowledge_frames = true;
        }
        effects
    }

    #[must_use]
    pub fn take_transcript_tail(&mut self) -> Vec<super::TranscriptEntry> {
        std::mem::take(&mut self.transcript)
            .into_iter()
            .filter(|entry| !entry.text.trim().is_empty())
            .collect()
    }

    #[must_use]
    pub fn close_effects(&self) -> BrowserVoiceEffects {
        BrowserVoiceEffects {
            frames: vec![json!({ "type": "session.close" }).to_string()],
            status: Some("Voice stopped".to_owned()),
            ..BrowserVoiceEffects::default()
        }
    }

    #[must_use]
    pub fn sideband_opened(&self) -> BrowserVoiceEffects {
        let frames = self.pending_frames.iter().cloned().collect::<Vec<_>>();
        BrowserVoiceEffects {
            acknowledge_frames: !frames.is_empty(),
            frames,
            ..BrowserVoiceEffects::default()
        }
    }

    #[must_use]
    pub fn sideband_closed(&mut self, connected_ms: u64) -> BrowserVoiceEffects {
        if connected_ms >= STABLE_CONNECTION_DURATION_MS {
            self.rapid_disconnects = 0;
        }
        self.rapid_disconnects = self.rapid_disconnects.saturating_add(1);
        BrowserVoiceEffects {
            reconnect_after_ms: Some(reconnect_delay_ms(self.rapid_disconnects)),
            status: Some("Voice reconnecting…".to_owned()),
            ..BrowserVoiceEffects::default()
        }
    }

    pub fn frames_sent(&mut self, count: usize) {
        for _ in 0..count.min(self.pending_frames.len()) {
            self.pending_frames.pop_front();
        }
    }

    fn push_output_frames(&mut self, output: &str, frames: &mut Vec<String>) {
        self.output_sent_this_run = true;
        for chunk in context_append_chunks(output) {
            if let Some(handoff_id) = self.active_delegation.as_deref() {
                let frame = json!({
                    "type": "delegation.context.append",
                    "delegation_item_id": handoff_id,
                    "content": [{ "type": "input_text", "text": chunk }],
                })
                .to_string();
                self.pending_frames.push_back(frame.clone());
                frames.push(frame);
            } else {
                let frame = json!({
                    "type": "session.context.append",
                    "content": [{ "type": "input_text", "text": chunk }],
                })
                .to_string();
                self.pending_frames.push_back(frame.clone());
                frames.push(frame);
            }
        }
    }
}

fn browser_voice_delegation(event: &Value) -> Option<(String, String)> {
    let item = event.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("delegation")
        || item.get("target").and_then(Value::as_str) != Some("client")
    {
        return None;
    }
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    let input = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    (!input.is_empty()).then_some((id, input))
}

fn reconnect_delay_ms(rapid_disconnects: u32) -> u64 {
    let exponent = rapid_disconnects.saturating_sub(1).min(63);
    RECONNECT_BASE_DELAY_MS
        .saturating_mul(1_u64 << exponent)
        .min(RECONNECT_MAX_DELAY_MS)
}

fn context_append_chunks(text: &str) -> Vec<&str> {
    if text.len() <= CONTEXT_APPEND_MAX_BYTES {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CONTEXT_APPEND_MAX_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

#[must_use]
pub fn preferred_physical_input(current_label: &str, labels: &[String]) -> Option<usize> {
    if current_label.is_empty() || !virtual_audio_input(current_label) {
        return None;
    }
    labels
        .iter()
        .position(|label| !virtual_audio_input(label) && built_in_audio_input(label))
        .or_else(|| labels.iter().position(|label| !virtual_audio_input(label)))
}

fn current_thread(history: &[VoiceHistoryEntry]) -> Option<String> {
    let mut turns: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    let mut user = Vec::new();
    let mut assistant = Vec::new();
    for entry in history {
        let text = entry.text.trim();
        if text.is_empty() || contextual(text) {
            continue;
        }
        match entry.role.as_str() {
            "user" => {
                if !user.is_empty() || !assistant.is_empty() {
                    turns.push((std::mem::take(&mut user), std::mem::take(&mut assistant)));
                }
                user.push(text.to_owned());
            }
            "assistant" if !user.is_empty() || !assistant.is_empty() => {
                assistant.push(text.to_owned());
            }
            _ => {}
        }
    }
    if !user.is_empty() || !assistant.is_empty() {
        turns.push((user, assistant));
    }
    if turns.is_empty() {
        return None;
    }
    let mut output = "Most recent user/assistant turns from this exact thread. Use them for continuity when responding.".to_owned();
    let mut remaining = CURRENT_THREAD_BUDGET.saturating_sub(tokens(&output));
    for (index, (user, assistant)) in turns.into_iter().rev().enumerate() {
        if remaining == 0 {
            break;
        }
        let mut rendered = if index == 0 {
            "### Latest turn".to_owned()
        } else {
            format!("### Previous turn {index}")
        };
        if !user.is_empty() {
            rendered.push_str("\nUser:\n");
            rendered.push_str(&user.join("\n\n"));
        }
        if !assistant.is_empty() {
            rendered.push_str("\n\nAssistant:\n");
            rendered.push_str(&assistant.join("\n\n"));
        }
        let rendered = truncate(&rendered, TURN_BUDGET.min(remaining));
        remaining = remaining.saturating_sub(tokens(&rendered));
        output.push_str("\n\n");
        output.push_str(&rendered);
    }
    Some(output)
}

fn workspace_map(path: &str, tree: &[String]) -> Option<String> {
    if path.trim().is_empty() && tree.is_empty() {
        return None;
    }
    let name = path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path);
    let mut lines = vec![
        format!("Current working directory: {path}"),
        format!("Working directory name: {name}"),
    ];
    if !tree.is_empty() {
        lines.extend([String::new(), "Working directory tree:".to_owned()]);
        lines.extend_from_slice(tree);
    }
    Some(lines.join("\n"))
}

fn contextual(text: &str) -> bool {
    text.starts_with("# AGENTS.md instructions")
        || [
            "<environment_context>",
            "<permissions instructions>",
            "<realtime_conversation>",
            "<turn_aborted>",
        ]
        .iter()
        .any(|marker| text.starts_with(marker))
}

fn section(parts: &mut Vec<String>, title: &str, body: Option<String>, budget: usize) {
    let Some(body) = body.filter(|body| !body.trim().is_empty()) else {
        return;
    };
    let heading = format!("## {title}\n");
    let body = truncate(&body, budget.saturating_sub(tokens(&heading)));
    if !body.is_empty() {
        parts.push(format!("{heading}{body}"));
    }
}

const fn tokens(text: &str) -> usize {
    text.len().div_ceil(APPROX_BYTES_PER_TOKEN)
}

fn truncate(text: &str, budget: usize) -> String {
    let maximum = budget.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= maximum {
        return text.to_owned();
    }
    let marker = "\n…truncated…\n";
    let keep = maximum.saturating_sub(marker.len());
    let head = take_first_bytes(text, keep / 2);
    let tail = take_last_bytes(text, keep.saturating_sub(head.len()));
    format!("{head}{marker}{tail}")
}

fn append_transcript(
    transcript: &mut Vec<super::TranscriptEntry>,
    role: &str,
    text: &str,
    force_new: bool,
) {
    if !force_new
        && let Some(last) = transcript.last_mut()
        && last.role == role
    {
        last.text.push_str(text);
    } else {
        transcript.push(super::TranscriptEntry::new(role, text));
    }
}

fn complete_transcript(
    transcript: &mut Vec<super::TranscriptEntry>,
    role: &str,
    text: &str,
    force_new: bool,
) {
    if !force_new
        && let Some(last) = transcript.last_mut()
        && last.role == role
    {
        last.text = text.to_owned();
    } else {
        transcript.push(super::TranscriptEntry::new(role, text));
    }
}

fn truncate_active_transcript(entries: &mut Vec<super::TranscriptEntry>) {
    let mut total_bytes = transcript_entries_bytes(entries);
    while total_bytes > MAX_ACTIVE_TRANSCRIPT_BYTES && entries.len() > 1 {
        total_bytes = total_bytes.saturating_sub(transcript_entry_bytes(&entries[0]));
        entries.remove(0);
    }
    let Some(entry) = entries.first_mut() else {
        return;
    };
    let entry_overhead = entry.role.len() + 3;
    let max_text_bytes = MAX_ACTIVE_TRANSCRIPT_BYTES.saturating_sub(entry_overhead);
    if entry.text.len() <= max_text_bytes {
        return;
    }
    let mut start = entry
        .text
        .len()
        .saturating_sub(max_text_bytes.saturating_sub(TRUNCATED_TRANSCRIPT_PREFIX.len()));
    while !entry.text.is_char_boundary(start) {
        start += 1;
    }
    entry.text = format!("{TRUNCATED_TRANSCRIPT_PREFIX}{}", &entry.text[start..]);
}

fn transcript_entries_bytes(entries: &[super::TranscriptEntry]) -> usize {
    entries.iter().map(transcript_entry_bytes).sum()
}

const fn transcript_entry_bytes(entry: &super::TranscriptEntry) -> usize {
    entry.role.len() + entry.text.len() + 3
}

fn payload_text(event: &Value) -> &str {
    event
        .pointer("/payload/text")
        .or_else(|| event.pointer("/payload/message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[derive(Default)]
struct HandoffStream {
    sent_bytes: usize,
    buffered_text: String,
    tail_text: String,
    truncated: bool,
}

impl HandoffStream {
    const fn stream_head_byte_limit(&self) -> usize {
        (REALTIME_OUTPUT_BYTE_LIMIT - HANDOFF_STREAM_TRUNCATION_MARKER.len()) / 2
    }

    const fn tail_byte_limit(&self) -> usize {
        REALTIME_OUTPUT_BYTE_LIMIT
            - self.stream_head_byte_limit()
            - HANDOFF_STREAM_TRUNCATION_MARKER.len()
    }

    const fn streamable_text_bytes(&self) -> usize {
        self.stream_head_byte_limit()
            .saturating_sub(self.sent_bytes)
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.truncated {
            self.tail_text.push_str(text);
            self.tail_text = take_last_bytes(&self.tail_text, self.tail_byte_limit()).to_owned();
            return;
        }
        self.buffered_text.push_str(text);
        let remaining = REALTIME_OUTPUT_BYTE_LIMIT.saturating_sub(self.sent_bytes);
        if self.buffered_text.len() <= remaining {
            return;
        }
        self.tail_text = take_last_bytes(&self.buffered_text, self.tail_byte_limit()).to_owned();
        self.buffered_text =
            take_first_bytes(&self.buffered_text, self.streamable_text_bytes()).to_owned();
        self.truncated = true;
    }

    fn drain_stream_chunk(&mut self) -> Option<String> {
        let split = take_first_bytes(&self.buffered_text, self.streamable_text_bytes()).len();
        if split == 0 {
            return None;
        }
        let text = self.buffered_text.drain(..split).collect::<String>();
        self.sent_bytes = self.sent_bytes.saturating_add(text.len());
        Some(text)
    }

    fn drain_final_chunk(&mut self) -> Option<String> {
        if !self.truncated {
            if self.buffered_text.is_empty() {
                return None;
            }
            let text = std::mem::take(&mut self.buffered_text);
            self.sent_bytes = self.sent_bytes.saturating_add(text.len());
            return Some(text);
        }
        let text = format!(
            "{}{HANDOFF_STREAM_TRUNCATION_MARKER}{}",
            std::mem::take(&mut self.buffered_text),
            std::mem::take(&mut self.tail_text)
        );
        self.sent_bytes = self.sent_bytes.saturating_add(text.len());
        Some(text)
    }
}

fn take_first_bytes(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn take_last_bytes(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn virtual_audio_input(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    [
        "blackhole",
        "soundflower",
        "loopback",
        "vb-audio",
        "virtual",
        "background music",
    ]
    .iter()
    .any(|part| label.contains(part))
}

fn built_in_audio_input(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    ["built-in", "macbook", "internal"]
        .iter()
        .any(|part| label.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_events_and_agent_output_are_owned_in_rust() {
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        let update = voice.realtime_message(r#"{"type":"delegation.created","item":{"type":"delegation","target":"client","id":"d1","content":[{"type":"input_text","text":"ship it"}]}}"#);
        assert_eq!(update.delegation.unwrap().input, "ship it");
        let effects = voice.agent_event(
            r#"{"type":"assistant.message","payload":{"phase":"final_answer","text":"done"}}"#,
        );
        assert!(effects.frames[0].contains("delegation.context.append"));
        assert!(effects.frames[0].contains("delegation_item_id"));
        assert!(effects.frames[0].contains("done"));
        assert!(!effects.frames[0].contains("[BACKEND]"));
    }

    #[test]
    fn v3_context_append_frames_use_codex_utf8_safe_five_hundred_byte_chunks() {
        let output = format!("{}{}", "a".repeat(499), "🦀".repeat(251));
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        let effects = voice.agent_event(
            &json!({
                "type": "assistant.message",
                "payload": { "text": output },
            })
            .to_string(),
        );
        let chunks = effects
            .frames
            .iter()
            .map(|frame| {
                let frame: Value = serde_json::from_str(frame).unwrap();
                frame["content"][0]["text"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= CONTEXT_APPEND_MAX_BYTES)
        );
        assert_eq!(chunks.concat(), output);
    }

    #[test]
    fn v3_provider_text_and_error_shapes_are_preserved() {
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        let update = voice.realtime_message(r#"{"type":"delegation.created","item":{"type":"delegation","target":"client","id":"d1","content":[{"type":"input_text","text":"  ship "},{"type":"input_text","text":"it  "}]}}"#);
        assert_eq!(update.delegation.unwrap().input, "  ship it  ");
        let error = voice.realtime_message(r#"{"type":"error","message":"root error"}"#);
        assert_eq!(
            error.effects.terminate.as_deref(),
            Some("Voice: root error")
        );
        let error = voice.realtime_message(r#"{"type":"error","error":{"code":"ended"}}"#);
        assert_eq!(
            error.effects.terminate.as_deref(),
            Some(r#"Voice: {"code":"ended"}"#),
        );
    }

    #[test]
    fn v3_sideband_reconnect_backoff_and_pending_frames_are_owned_in_rust() {
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        let effects =
            voice.agent_event(r#"{"type":"assistant.message","payload":{"text":"pending"}}"#);
        assert_eq!(voice.sideband_opened().frames, effects.frames);
        voice.frames_sent(1);
        assert!(voice.sideband_opened().frames.is_empty());
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(200));
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(400));
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(800));
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(1_600));
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(3_200));
        assert_eq!(voice.sideband_closed(1_000).reconnect_after_ms, Some(5_000));
        assert_eq!(voice.sideband_closed(30_000).reconnect_after_ms, Some(200));
    }

    #[test]
    fn startup_context_and_device_policy_are_bounded_in_rust() {
        let context = build_browser_startup_context(
            &[VoiceHistoryEntry::new("user", "build voice")],
            "/workspace",
            &["- src/".to_owned(), "  - lib.rs".to_owned()],
        )
        .unwrap();
        assert!(context.contains("build voice"));
        assert!(context.contains("src/"));
        assert!(context.len() <= TOTAL_BUDGET * APPROX_BYTES_PER_TOKEN);
        assert_eq!(
            preferred_physical_input(
                "BlackHole 2ch (Virtual)",
                &["USB Mic".to_owned(), "MacBook Pro Microphone".to_owned()]
            ),
            Some(1)
        );
    }

    #[test]
    fn chatgpt_call_body_is_built_entirely_in_rust() {
        let call = build_chatgpt_realtime_call("v=offer", "cove", Some("<startup />")).unwrap();
        let call: Value = serde_json::from_str(&call).unwrap();
        assert_eq!(call["sdp"], "v=offer");
        assert!(call["session"].get("type").is_none());
        assert_eq!(call["session"]["model"], CHATGPT_REALTIME_MODEL);
        assert!(call["session"]["audio"].get("input").is_none());
        assert_eq!(call["session"]["audio"]["output"]["voice"], "cove");
        assert!(
            call["session"]["instructions"]
                .as_str()
                .unwrap()
                .ends_with("\n\n<startup />")
        );
    }

    #[test]
    fn only_valid_rust_decoded_delegations_require_agent_admission() {
        assert!(realtime_message_requires_agent_admission(
            r#"{"type":"delegation.created","item":{"type":"delegation","target":"client","id":"d1","content":[{"type":"input_text","text":"ship it"}]}}"#,
        ));
        assert!(!realtime_message_requires_agent_admission(
            r#"{"type":"delegation.created","item":{"type":"delegation","target":"server","id":"d1","content":[{"type":"input_text","text":"ship it"}]}}"#,
        ));
        assert!(!realtime_message_requires_agent_admission(
            r#"{"type":"turn.done"}"#,
        ));
    }

    #[test]
    fn call_response_and_location_are_decoded_entirely_in_rust() {
        let result = decode_chatgpt_realtime_call(
            "v=answer\r\n",
            "/v1/live/019eb97d-8e9a-7ff3-94b0-ea019babd5d7?trace=1",
        )
        .unwrap();
        assert_eq!(result.sdp, "v=answer\r\n");
        assert_eq!(result.call_id, "019eb97d-8e9a-7ff3-94b0-ea019babd5d7");
        assert!(decode_chatgpt_realtime_call("v=answer", "/v1/live").is_err());
    }

    #[test]
    fn active_transcript_uses_codex_eight_kibibyte_bound() {
        let mut voice = BrowserVoiceProtocol::new("cove").unwrap();
        let payload = json!({
            "type": "input_transcript.added",
            "item": { "text": "x".repeat(MAX_ACTIVE_TRANSCRIPT_BYTES * 2) },
        });
        let _ = voice.realtime_message(&payload.to_string());
        let tail = voice.take_transcript_tail();
        assert!(transcript_entries_bytes(&tail) <= MAX_ACTIVE_TRANSCRIPT_BYTES);
        assert!(tail[0].text.starts_with(TRUNCATED_TRANSCRIPT_PREFIX));
    }
}
