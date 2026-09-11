//! Bounded live captions and independently sampled audio meters for the composer strip.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nanocodex_voice::VoiceSpeaker;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

// Timing and tile sampling follow upstream realtime_split_flap.rs (Apache-2.0).
#[derive(Default)]
struct Caption {
    text: String,
    arrivals: Vec<Instant>,
    phase: Option<Instant>,
}

impl Caption {
    fn append(&mut self, delta: &str, now: Instant) {
        self.phase.get_or_insert(now);
        self.text.push_str(delta);
        self.arrivals.extend(
            (0..delta.len()).map(|index| now + Duration::from_millis((index as u64 * 8).min(56))),
        );
        if self.text.len() > 1024 {
            let mut start = self.text.len() - 1024;
            while !self.text.is_char_boundary(start) {
                start += 1;
            }
            self.text.drain(..start);
            self.arrivals.drain(..start);
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn line(&self, prefix: &str, animated: bool, color: Color) -> Line<'static> {
        self.line_at(prefix, animated, color, Instant::now())
    }

    fn line_at(&self, prefix: &str, animated: bool, color: Color, now: Instant) -> Line<'static> {
        let mut sample = self
            .text
            .bytes()
            .rev()
            .filter(u8::is_ascii_alphabetic)
            .take(24)
            .collect::<Vec<_>>();
        if sample.is_empty() {
            sample.extend(b"0123456789");
        }
        sample.reverse();
        let phase = now
            .saturating_duration_since(self.phase.unwrap_or(now))
            .as_millis()
            / 45;
        let mut spans = vec![Span::styled(prefix.to_owned(), Style::default().fg(color))];
        for (index, character) in self.text.char_indices() {
            let elapsed = now.saturating_duration_since(self.arrivals[index]);
            let flipping = animated
                && character.is_ascii_alphanumeric()
                && elapsed < Duration::from_millis(180);
            let glyph = if flipping {
                if elapsed < Duration::from_millis(40) {
                    ' '
                } else {
                    char::from(
                        sample
                            [(character as usize + index * 7 + phase as usize * 11) % sample.len()],
                    )
                }
            } else {
                character
            };
            let foreground = if flipping {
                Color::DarkGray
            } else if animated && elapsed < Duration::from_millis(315) {
                color
            } else {
                Color::Gray
            };
            spans.push(Span::styled(
                glyph.to_string(),
                Style::default().fg(foreground),
            ));
        }
        Line::from(spans)
    }
}

pub(super) struct VoiceUi {
    pub active: bool,
    generation: u64,
    pub connecting: bool,
    pub muted: bool,
    pub microphone: u16,
    pub speaker: u16,
    user: Caption,
    assistant: Caption,
    pub animations: bool,
    recovered_answers: VecDeque<u64>,
    captioned_answers: VecDeque<u64>,
    pub mute_key: String,
}

impl Default for VoiceUi {
    fn default() -> Self {
        Self {
            active: false,
            generation: 0,
            connecting: false,
            muted: false,
            microphone: 0,
            speaker: 0,
            user: Caption::default(),
            assistant: Caption::default(),
            animations: true,
            recovered_answers: VecDeque::new(),
            captioned_answers: VecDeque::new(),
            mute_key: "ctrl+x".into(),
        }
    }
}

impl VoiceUi {
    pub fn accept_generation(&mut self, generation: u64, recovered: bool) -> bool {
        if generation < self.generation {
            return recovered;
        }
        self.generation = generation;
        true
    }

    pub fn record_answer(&mut self, text: &str, recovered: bool) -> bool {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        let text = text.trim().strip_prefix("[FINAL]").unwrap_or(text.trim());
        for word in text.split_whitespace() {
            word.hash(&mut hash);
        }
        let fingerprint = hash.finish();
        let (own, other) = if recovered {
            (&mut self.recovered_answers, &mut self.captioned_answers)
        } else {
            (&mut self.captioned_answers, &mut self.recovered_answers)
        };
        if let Some(index) = other.iter().position(|value| *value == fingerprint) {
            other.remove(index);
            return false;
        }
        if own.len() == 64 {
            own.pop_front();
        }
        own.push_back(fingerprint);
        true
    }
    pub fn visible(&self) -> bool {
        self.active || self.connecting
    }
    pub fn delta(&mut self, speaker: VoiceSpeaker, delta: &str) {
        let text = match speaker {
            VoiceSpeaker::User => &mut self.user,
            VoiceSpeaker::Assistant => &mut self.assistant,
        };
        text.append(delta, Instant::now());
    }
    pub fn complete(&mut self, speaker: VoiceSpeaker) {
        match speaker {
            VoiceSpeaker::User => self.user.clear(),
            VoiceSpeaker::Assistant => self.assistant.clear(),
        }
    }
    pub fn stop(&mut self) -> [(VoiceSpeaker, String); 2] {
        self.active = false;
        self.connecting = false;
        self.microphone = 0;
        self.speaker = 0;
        [
            (VoiceSpeaker::User, std::mem::take(&mut self.user).text),
            (
                VoiceSpeaker::Assistant,
                std::mem::take(&mut self.assistant).text,
            ),
        ]
    }
    pub fn matches_mute(&self, event: KeyEvent) -> bool {
        self.visible()
            && parse_key(&self.mute_key)
                .is_some_and(|(code, modifiers)| event.code == code && event.modifiers == modifiers)
    }
}

pub(super) fn parse_key(value: &str) -> Option<(KeyCode, KeyModifiers)> {
    let value = value.to_ascii_lowercase();
    let mut parts = value.split('+');
    let modifier = match parts.next()? {
        "ctrl" => KeyModifiers::CONTROL,
        "alt" => KeyModifiers::ALT,
        _ => return None,
    };
    let key = parts.next()?;
    let mut chars = key.chars();
    let character = chars.next()?;
    (chars.next().is_none() && parts.next().is_none())
        .then_some((KeyCode::Char(character), modifier))
}

pub(crate) fn validate_key(value: &str) -> Result<String, String> {
    if value == "none" || parse_key(value).is_some() {
        Ok(value.to_owned())
    } else {
        Err("use ctrl+<character>, alt+<character>, or none".into())
    }
}

fn meter(peak: u16) -> String {
    let count = if peak < 512 {
        0
    } else {
        (usize::from(peak.min(8192)) * 5 / 8192).max(1)
    };
    format!("{}{}", "▮".repeat(count), "·".repeat(5 - count))
}

pub(super) fn render(frame: &mut Frame<'_>, state: &VoiceUi, area: Rect) {
    if !state.visible() {
        return;
    }
    let status = if state.connecting {
        "Connecting"
    } else if state.muted {
        "Muted"
    } else if state.speaker > 512 {
        "Speaking"
    } else {
        "Listening"
    };
    let binding = if state.mute_key == "none" {
        "/voice mute"
    } else {
        &state.mute_key
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(format!(" {status}  "), Style::default().fg(Color::Cyan)),
            Span::raw(format!(
                "mic {}  speaker {}  · {binding} mute · /voice stop",
                meter(if state.muted { 0 } else { state.microphone }),
                meter(state.speaker)
            )),
        ]),
        state.user.line(" You: ", state.animations, Color::Cyan),
        state
            .assistant
            .line(" Voice: ", state.animations, Color::Magenta),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_session_cannot_stop_or_unmute_its_replacement() {
        let mut state = VoiceUi::default();
        assert!(state.accept_generation(1, false));
        assert!(state.accept_generation(3, false));
        assert!(!state.accept_generation(1, false));
        assert!(state.accept_generation(1, true));
        assert!(!state.accept_generation(2, false));
    }
    #[test]
    fn late_caption_does_not_duplicate_a_recovered_final_answer() {
        let mut state = VoiceUi::default();
        assert!(state.record_answer("[FINAL] The answer is 42.", true));
        assert!(!state.record_answer("The answer  is\n42.", false));
        assert!(state.record_answer("Another answer", false));
        assert!(state.record_answer("Another answer", false));
    }
    #[test]
    fn split_flap_settles_and_reduced_motion_preserves_unicode() {
        let mut caption = Caption::default();
        let now = Instant::now();
        caption.append("Hello 世界", now);
        let text = |line: Line<'static>| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        };
        assert_eq!(
            text(caption.line_at("", false, Color::Cyan, now)),
            "Hello 世界"
        );
        assert_ne!(
            text(caption.line_at("", true, Color::Cyan, now)),
            "Hello 世界"
        );
        assert_eq!(
            text(caption.line_at("", true, Color::Cyan, now + Duration::from_secs(1))),
            "Hello 世界"
        );
    }
    #[test]
    fn interleaved_captions_remain_bounded_and_stop_recovers_both() {
        let mut state = VoiceUi {
            active: true,
            ..Default::default()
        };
        state.delta(VoiceSpeaker::User, &"é".repeat(800));
        state.delta(VoiceSpeaker::Assistant, "answer");
        assert_eq!(state.user.text.len(), 1024);
        let tails = state.stop();
        assert_eq!(tails[1].1, "answer");
        assert!(!state.visible());
    }
    #[test]
    fn configurable_mute_is_inactive_outside_voice() {
        let mut state = VoiceUi::default();
        let event = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(!state.matches_mute(event));
        state.active = true;
        assert!(state.matches_mute(event));
        state.mute_key = "none".into();
        assert!(!state.matches_mute(event));
        assert!(validate_key("ctrl+xx").is_err());
    }
}
