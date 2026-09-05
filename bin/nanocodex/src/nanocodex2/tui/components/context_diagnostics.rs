// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Read-only context telemetry overlay.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    context::{CompactionDiagnostics, CompactionTrigger, ContextDiagnostics, ContinuationMode},
    theme::Theme,
};
use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

const FOOTER: [(&str, &str); 2] = [("r", "refresh"), ("esc", "close")];

pub(super) enum ContextDiagnosticsEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ContextDiagnosticsEffect {
    Dismiss,
    Refresh,
}

pub(super) struct ContextDiagnosticsPanel {
    diagnostics: ContextDiagnostics,
}

impl ContextDiagnosticsPanel {
    pub(super) const fn new(diagnostics: ContextDiagnostics) -> Self {
        Self { diagnostics }
    }

    pub(super) fn replace(&mut self, diagnostics: ContextDiagnostics) {
        self.diagnostics = diagnostics;
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let label = Style::default().fg(theme.muted());
        let value = Style::default().fg(theme.text());
        let heading = Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD);
        let mut lines = vec![Line::styled(" Context budget", heading)];
        let usage = self.diagnostics.usage;
        let current = usage.map(|usage| usage.total);
        lines.extend([
            fact(
                " Window / auto compact",
                format!(
                    "{} / {}",
                    format_count(self.diagnostics.model_window_tokens),
                    format_count(self.diagnostics.auto_compact_token_limit)
                ),
                label,
                value,
            ),
            fact(
                " Current / headroom",
                current.map_or_else(
                    || "unavailable".to_owned(),
                    |tokens| {
                        format!(
                            "{} / {}",
                            format_count(tokens),
                            format_count(
                                self.diagnostics.model_window_tokens.saturating_sub(tokens)
                            )
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Until auto compact",
                optional_count(current.map(|tokens| {
                    self.diagnostics
                        .auto_compact_token_limit
                        .saturating_sub(tokens)
                })),
                label,
                value,
            ),
            Line::styled(" Latest server usage", heading),
            fact(
                " Input (cached/uncached)",
                usage.map_or_else(
                    || "unavailable".to_owned(),
                    |usage| {
                        format!(
                            "{} ({}/{})",
                            format_count(usage.input),
                            format_count(usage.cached_input),
                            format_count(usage.uncached_input)
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Output / total",
                usage.map_or_else(
                    || "unavailable".to_owned(),
                    |usage| {
                        format!(
                            "{} / {}",
                            format_count(usage.output),
                            format_count(usage.total)
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Cached-token effect",
                "cached input still counts toward the window".to_owned(),
                label,
                value,
            ),
            Line::styled(" Generation", heading),
            fact(
                " Continuation",
                match self.diagnostics.continuation {
                    Some(ContinuationMode::FullContext) => "full context".to_owned(),
                    Some(ContinuationMode::PreviousResponse) => "previous response".to_owned(),
                    None => "unavailable".to_owned(),
                },
                label,
                value,
            ),
            fact(
                " Prompt cache",
                optional_bool(self.diagnostics.prompt_cache),
                label,
                value,
            ),
            Line::styled(" Local estimates", heading),
            fact(
                " Context categories",
                "unavailable (prefix/user/assistant/reasoning/tools/compaction/media)".to_owned(),
                label,
                value,
            ),
            fact(
                " Pending shell",
                "unavailable (count/bytes/tokens)".to_owned(),
                label,
                value,
            ),
            fact(
                " Server/local delta",
                "unavailable".to_owned(),
                label,
                value,
            ),
            Line::styled(" Compaction", heading),
            fact(
                " Started / completed",
                format!(
                    "{} / {}",
                    self.diagnostics.compactions_started, self.diagnostics.compactions_completed
                ),
                label,
                value,
            ),
        ]);
        let compaction = self.diagnostics.last_compaction;
        lines.extend([
            fact(
                " Trigger / time",
                compaction.map_or_else(|| "unavailable".to_owned(), format_compaction_time),
                label,
                value,
            ),
            fact(
                " Before / next input",
                compaction.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}",
                            optional_count(item.before_tokens),
                            optional_count(item.after_tokens)
                        )
                    },
                ),
                label,
                value,
            ),
        ]);
        lines
    }
}

impl Component for ContextDiagnosticsPanel {
    type Event = ContextDiagnosticsEvent;
    type Effect = ContextDiagnosticsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let ContextDiagnosticsEvent::Terminal(Event::Key(key)) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        let effect = match key.code {
            KeyCode::Esc => ContextDiagnosticsEffect::Dismiss,
            KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                ContextDiagnosticsEffect::Refresh
            }
            _ => return ComponentUpdate::none(),
        };
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let layout =
            Floating::new("Context diagnostics", 78, 28, &FOOTER).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        frame.render_widget(
            Paragraph::new(self.lines(theme)).wrap(Wrap { trim: false }),
            layout.body,
        );
    }
}

fn fact(
    label_text: &'static str,
    value_text: String,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label_text:<26}"), label_style),
        Span::styled(value_text, value_style),
    ])
}

fn optional_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), format_count)
}

fn optional_bool(value: Option<bool>) -> String {
    value.map_or_else(
        || "unavailable".to_owned(),
        |present| if present { "present" } else { "absent" }.to_owned(),
    )
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

fn format_compaction_time(compaction: CompactionDiagnostics) -> String {
    let trigger = match compaction.trigger {
        CompactionTrigger::Automatic => "automatic",
    };
    let timestamp = i64::try_from(compaction.started_at_unix_ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(
            || "unknown time".to_owned(),
            |time| time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        );
    let duration = compaction.completed_at_unix_ms.map_or_else(
        || "ongoing".to_owned(),
        |completed| format_duration_millis(completed.saturating_sub(compaction.started_at_unix_ms)),
    );
    format!("{trigger} / {timestamp} · {duration}")
}

fn format_duration_millis(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        return format!("{milliseconds}ms");
    }
    format!("{}.{}s", milliseconds / 1_000, milliseconds % 1_000 / 100)
}

#[cfg(test)]
mod tests {
    use super::{Component, ContextDiagnosticsPanel, format_compaction_time};
    use crate::tui::{
        context::{
            CompactionDiagnostics, CompactionTrigger, ContextDiagnostics, ContinuationMode,
            TokenUsage,
        },
        theme::Theme,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn panel_renders_counts_unavailable_metrics_and_cache_help() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.usage = Some(TokenUsage {
            input: 100_000,
            cached_input: 75_000,
            uncached_input: 25_000,
            output: 2_000,
            total: 102_000,
        });
        diagnostics.continuation = Some(ContinuationMode::PreviousResponse);
        diagnostics.prompt_cache = Some(true);
        let mut panel = ContextDiagnosticsPanel::new(diagnostics);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Context diagnostics",
            "100,000 (75,000/25,000)",
            "previous response",
            "Context categories",
            "Pending shell",
            "count/bytes/tokens",
            "cached input still counts toward the window",
            "r refresh · esc close",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}\n{rendered}"
            );
        }
    }

    #[test]
    fn compaction_time_is_readable_and_includes_duration() {
        let rendered = format_compaction_time(CompactionDiagnostics {
            trigger: CompactionTrigger::Automatic,
            started_at_unix_ms: 0,
            completed_at_unix_ms: Some(39_095),
            before_tokens: None,
            after_tokens: None,
        });

        assert_eq!(rendered, "automatic / 1970-01-01 00:00:00Z · 39.0s");
    }
}
