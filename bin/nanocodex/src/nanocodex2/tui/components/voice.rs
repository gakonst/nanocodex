//! Composer voice strip, following the original Nanocodex and Codex TUI controls.
use crate::voice_state::{Phase, Status};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

fn meter(peak: u16) -> String {
    let bars = if peak < 512 {
        0
    } else {
        (usize::from(peak.min(8192)) * 5 / 8192).max(1)
    };
    format!("{}{}", "▮".repeat(bars), "·".repeat(5 - bars))
}

pub(super) fn render(frame: &mut Frame<'_>, state: &Status, area: Rect) {
    if state.text.starts_with("Voice clone:") {
        let recording = state
            .text
            .lines()
            .find(|line| line.starts_with("● RECORDING"));
        let label = recording.map_or_else(
            || state.text.lines().take(2).collect::<Vec<_>>().join(" · "),
            |line| format!(" {line} · S stop · Esc cancel"),
        );
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(if recording.is_some() {
                Color::Red
            } else {
                Color::Yellow
            })),
            area,
        );
        return;
    }
    let phase = match state.phase {
        Phase::Connecting => "Connecting",
        Phase::Stopping => "Stopping",
        Phase::Active if state.speaking => "Speaking",
        Phase::Active if state.muted => "Muted",
        Phase::Active => "Listening",
    };
    let binding = if state.muted { "unmute" } else { "mute" };
    let microphone = meter(if state.muted { 0 } else { state.microphone });
    let speaker = meter(state.speaker);
    let lines = vec![Line::from(vec![
        Span::styled(format!(" {phase}  "), Style::default().fg(Color::Cyan)),
        Span::raw(format!(
            "mic {}  speaker {} · ctrl+x {binding} · /voice off",
            microphone, speaker
        )),
    ])];
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn elevenlabs_uses_native_microphone_and_speaker_meters() {
        let state = Status {
            text: "ElevenLabs test_voice speaking".into(),
            phase: Phase::Active,
            speaking: true,
            microphone: 8192,
            speaker: 8192,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        terminal
            .draw(|frame| render(frame, &state, frame.area()))
            .unwrap();
        let line: String = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(line.contains("Speaking"));
        assert!(line.contains("mic ▮▮▮▮▮"));
        assert!(line.contains("speaker ▮▮▮▮▮"));
        assert!(!line.contains("Listening"));
    }

    #[test]
    fn recording_strip_shows_timer_and_signal_instead_of_only_clone_name() {
        let state = Status {
            text: "Voice clone: Sample\n● RECORDING  00:03 / 02:00   mic [▮▮··········]".into(),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        terminal
            .draw(|frame| render(frame, &state, frame.area()))
            .unwrap();
        let line: String = (0..100)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(line.contains("RECORDING  00:03"));
        assert!(line.contains("mic [▮▮"));
        assert!(line.contains("S stop"));
    }
}
