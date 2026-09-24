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
