//! Shared, transport-neutral Codex Realtime adapter policy.

mod browser;

/// Exact Codex Realtime side-agent instructions before user-name substitution.
pub const CHATGPT_REALTIME_BACKEND_PROMPT_TEMPLATE: &str = include_str!("backend_prompt.md");

pub use browser::{
    BrowserRealtimeCallResult, BrowserVoiceEffects, BrowserVoiceProtocol, BrowserVoiceUpdate,
    CHATGPT_REALTIME_MODEL, CHATGPT_REALTIME_VOICE, CHATGPT_REALTIME_VOICES, VoiceHistoryEntry,
    build_browser_startup_context, build_chatgpt_realtime_call, decode_chatgpt_realtime_call,
    preferred_physical_input, realtime_message_requires_agent_admission, valid_realtime_call_id,
};

/// Builds the exact Codex Realtime side-agent instructions.
#[must_use]
pub fn chatgpt_realtime_instructions(user_first_name: &str) -> String {
    CHATGPT_REALTIME_BACKEND_PROMPT_TEMPLATE.replace("{{ user_first_name }}", user_first_name)
}

/// Canonical developer marker appended when a Realtime conversation begins.
pub const REALTIME_START_INSTRUCTIONS: &str = concat!(
    "<realtime_conversation>\n\n",
    "Realtime conversation started.\n\n",
    "You are operating as a backend executor behind an intermediary. The user does not talk to you directly. Any response you produce will be consumed by the intermediary and may be summarized before the user sees it.\n\n",
    "When invoked, you receive the latest conversation transcript and any relevant mode or metadata. The intermediary may invoke you even when backend help is not actually needed. Use the transcript to decide whether you should do work. If backend help is unnecessary, avoid verbose responses that add user-visible latency.\n\n",
    "When user text is routed from realtime, treat it as a transcript. It may be unpunctuated or contain recognition errors.\n\n",
    "- Keep responses concise and action-oriented. Your updates should help the intermediary respond to the user.\n\n",
    "</realtime_conversation>"
);

/// Canonical developer marker appended when a Realtime conversation ends.
pub const REALTIME_END_INSTRUCTIONS: &str = concat!(
    "<realtime_conversation>\n\n",
    "Realtime conversation ended.\n\n",
    "Subsequent user input will return to typed text rather than transcript-style text. Do not assume recognition errors or missing punctuation once realtime has ended. Resume normal chat behavior.\n\n",
    "Reason: inactive\n\n",
    "</realtime_conversation>"
);

const REALTIME_SESSION_ENDED_HANDOFF_INSTRUCTION: &str = "The user just ended their realtime session. Here is the remaining handoff/transcript tail. You probably do not have to do anything; acknowledge the handoff unless the transcript itself asks for something.";
const MAX_REALTIME_DELEGATION_FIELD_BYTES: usize = 4 * 1024;
const TRUNCATION_MARKER: &str = "…";

/// One completed user or assistant transcript entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEntry {
    /// Stable participant label understood by the Realtime prompt.
    pub role: String,
    /// Completed transcript text.
    pub text: String,
}

impl TranscriptEntry {
    /// Creates one role-bearing transcript entry.
    #[must_use]
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
        }
    }
}

/// Wraps delegated speech and its new transcript using canonical Codex markers.
#[must_use]
pub fn realtime_delegation(input: &str, transcript: &[TranscriptEntry]) -> String {
    let input = escape_xml_bounded(input, Retain::Start);
    let transcript = transcript_text(transcript);
    if transcript.is_empty() {
        format!("<realtime_delegation>\n  <input>{input}</input>\n</realtime_delegation>")
    } else {
        format!(
            "<realtime_delegation>\n  <input>{input}</input>\n  <transcript_delta>{transcript}</transcript_delta>\n</realtime_delegation>"
        )
    }
}

/// Wraps an unconsumed transcript tail using canonical Codex markers.
#[must_use]
pub fn realtime_tail_delegation(transcript: &[TranscriptEntry]) -> Option<String> {
    if transcript.is_empty() {
        return None;
    }
    let input = escape_xml_bounded(REALTIME_SESSION_ENDED_HANDOFF_INSTRUCTION, Retain::Start);
    let transcript = transcript_text(transcript);
    Some(format!(
        "<realtime_delegation>\n  <source>transcript_tail_flush</source>\n  <input>{input}</input>\n  <transcript_delta>{transcript}</transcript_delta>\n</realtime_delegation>"
    ))
}

fn transcript_text(transcript: &[TranscriptEntry]) -> String {
    escape_xml_bounded(
        &transcript
            .iter()
            .map(|entry| format!("{}: {}", entry.role, entry.text))
            .collect::<Vec<_>>()
            .join("\n"),
        Retain::End,
    )
}

#[derive(Clone, Copy)]
enum Retain {
    Start,
    End,
}

fn escape_xml_bounded(text: &str, retain: Retain) -> String {
    let escaped = escape_xml(text);
    if escaped.len() <= MAX_REALTIME_DELEGATION_FIELD_BYTES {
        return escaped;
    }
    let retained_bytes = MAX_REALTIME_DELEGATION_FIELD_BYTES - TRUNCATION_MARKER.len();
    match retain {
        Retain::Start => {
            let mut end = retained_bytes;
            while !escaped.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}{TRUNCATION_MARKER}", &escaped[..end])
        }
        Retain::End => {
            let mut start = escaped.len().saturating_sub(retained_bytes);
            while !escaped.is_char_boundary(start) {
                start += 1;
            }
            format!("{TRUNCATION_MARKER}{}", &escaped[start..])
        }
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_REALTIME_DELEGATION_FIELD_BYTES, TranscriptEntry, realtime_delegation,
        realtime_tail_delegation, transcript_text,
    };

    #[test]
    fn delegation_escapes_structured_input() {
        assert_eq!(
            realtime_delegation(
                "fix <x> & ship",
                &[TranscriptEntry::new("user", "yes & now")],
            ),
            "<realtime_delegation>\n  <input>fix &lt;x&gt; &amp; ship</input>\n  <transcript_delta>user: yes &amp; now</transcript_delta>\n</realtime_delegation>"
        );
    }

    #[test]
    fn empty_tail_is_not_routed() {
        assert_eq!(realtime_tail_delegation(&[]), None);
    }

    #[test]
    fn delegation_fields_keep_the_codex_bounded_edge() {
        let input = "start".to_owned() + &"x".repeat(MAX_REALTIME_DELEGATION_FIELD_BYTES) + "end";
        let delegation = realtime_delegation(&input, &[]);
        assert!(delegation.contains("<input>start"));
        assert!(!delegation.contains("end</input>"));

        let transcript = transcript_text(&[TranscriptEntry::new("user", input)]);
        assert!(transcript.len() <= MAX_REALTIME_DELEGATION_FIELD_BYTES);
        assert!(transcript.starts_with('…'));
        assert!(transcript.ends_with("end"));

        let unicode = "é".repeat(MAX_REALTIME_DELEGATION_FIELD_BYTES);
        let unicode = transcript_text(&[TranscriptEntry::new("assistant", unicode)]);
        assert!(unicode.len() <= MAX_REALTIME_DELEGATION_FIELD_BYTES);
        assert!(unicode.starts_with('…'));
        assert!(unicode.is_char_boundary(unicode.len()));
    }
}
