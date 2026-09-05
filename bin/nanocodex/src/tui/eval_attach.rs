use std::{io, path::Path, time::Duration};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use eyre::{Result, WrapErr as _};
use futures_util::StreamExt as _;
use nanocodex_eval::{EvaluationFamilyStatus, EvaluationObserver, EvaluationStatus};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table},
};
use tokio::time::{MissedTickBehavior, interval};

use super::terminal::TerminalSession;

const SQLITE_PROBE_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) async fn attach_evaluation(mut observer: EvaluationObserver) -> Result<()> {
    let snapshot = observer
        .snapshot()
        .wrap_err("failed to read the initial evaluation snapshot")?;
    let mut view = AttachView::new(snapshot);
    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize the terminal")?;
    let mut input = EventStream::new();
    let mut probe = interval(SQLITE_PROBE_INTERVAL);
    probe.set_missed_tick_behavior(MissedTickBehavior::Skip);
    probe.tick().await;
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| render(frame, &mut view))?;
            dirty = false;
        }
        tokio::select! {
            event = input.next() => {
                let event = event.transpose()?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "terminal input closed")
                })?;
                match view.handle_event(event) {
                    AttachAction::Continue { redraw } => dirty |= redraw,
                    AttachAction::Refresh => {
                        view.replace(observer.snapshot()?);
                        dirty = true;
                    }
                    AttachAction::Quit => return Ok(()),
                }
            }
            _ = probe.tick() => {
                if let Some(snapshot) = observer.refresh()? {
                    view.replace(snapshot);
                    dirty = true;
                }
            }
        }
    }
}

struct AttachView {
    snapshot: EvaluationStatus,
    offset: usize,
    page_size: usize,
}

impl AttachView {
    const fn new(snapshot: EvaluationStatus) -> Self {
        Self {
            snapshot,
            offset: 0,
            page_size: 1,
        }
    }

    fn replace(&mut self, snapshot: EvaluationStatus) {
        self.snapshot = snapshot;
        self.clamp_offset();
    }

    fn handle_event(&mut self, event: Event) -> AttachAction {
        let key = match event {
            Event::Key(key) => key,
            Event::Resize(_, _) => return AttachAction::Continue { redraw: true },
            _ => return AttachAction::Continue { redraw: false },
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return AttachAction::Continue { redraw: false };
        }
        if is_quit(key) {
            return AttachAction::Quit;
        }
        if matches!(key.code, KeyCode::Char('r')) {
            return AttachAction::Refresh;
        }
        let previous = self.offset;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.offset = self.offset.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.offset = self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(self.page_size),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(self.page_size),
            KeyCode::Home => self.offset = 0,
            KeyCode::End => {
                self.offset = self.snapshot.families.len().saturating_sub(self.page_size)
            }
            _ => {}
        }
        self.clamp_offset();
        AttachAction::Continue {
            redraw: self.offset != previous,
        }
    }

    fn clamp_offset(&mut self) {
        self.offset = self
            .offset
            .min(self.snapshot.families.len().saturating_sub(self.page_size));
    }
}

enum AttachAction {
    Continue { redraw: bool },
    Refresh,
    Quit,
}

const fn is_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')))
}

fn render(frame: &mut Frame<'_>, view: &mut AttachView) {
    let area = frame.area();
    let [header, progress, summary, families, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    let page_size = usize::from(families.height.saturating_sub(3)).max(1);
    view.page_size = page_size;
    view.clamp_offset();
    let status = &view.snapshot;
    let digest = &status.digest[..status.digest.len().min(12)];
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", status.profile),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(digest, Style::default().fg(Color::DarkGray)),
        ])),
        header,
    );

    let total = status.tasks.total().max(0) as u64;
    let complete = status.tasks.finished().max(0) as u64;
    let ratio = if total == 0 {
        0.0
    } else {
        complete as f64 / total as f64
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio.clamp(0.0, 1.0))
            .label(format!("{complete}/{total}  {:.1}%", ratio * 100.0))
            .gauge_style(Style::default().fg(Color::Green)),
        progress,
    );

    frame.render_widget(
        Paragraph::new(vec![Line::from(format!(
            " tasks  {} running  ·  {} unclaimed  ·  {} success  ·  {} failed",
            status.tasks.running, status.tasks.unclaimed, status.tasks.success, status.tasks.failed,
        ))]),
        summary,
    );

    let rows = status
        .families
        .iter()
        .skip(view.offset)
        .take(page_size)
        .map(family_row);
    let title = format!(
        " Families {}-{} of {} ",
        view.offset.saturating_add(1).min(status.families.len()),
        view.offset
            .saturating_add(page_size)
            .min(status.families.len()),
        status.families.len()
    );
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Percentage(12),
            Constraint::Percentage(10),
            Constraint::Percentage(10),
            Constraint::Percentage(16),
            Constraint::Percentage(18),
        ],
    )
    .header(
        Row::new([
            "task",
            "harness",
            "model",
            "thinking",
            "state",
            "ok/fail/run/new",
        ])
        .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title(title).borders(Borders::ALL))
    .column_spacing(1);
    frame.render_widget(table, families);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" read-only SQLite ", Style::default().fg(Color::DarkGray)),
            Span::raw("· ↑↓/jk scroll · r refresh · q quit"),
        ])),
        footer,
    );
}

fn family_row(family: &EvaluationFamilyStatus) -> Row<'static> {
    let style = if family.unclaimed == 0 && family.running == 0 && family.failed == 0 {
        Style::default().fg(Color::Green)
    } else if family.failed > 0 {
        Style::default().fg(Color::Red)
    } else if family.running > 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    Row::new([
        Cell::from(task_label(&family.task)),
        Cell::from(family.treatment.harness.clone()),
        Cell::from(model_label(family)),
        Cell::from(family.treatment.thinking.to_string()),
        Cell::from(if family.running > 0 {
            "running"
        } else if family.unclaimed > 0 {
            "unclaimed"
        } else if family.failed > 0 {
            "failed"
        } else {
            "success"
        }),
        Cell::from(format!(
            "{}/{}/{}/{}",
            family.success, family.failed, family.running, family.unclaimed
        )),
    ])
    .style(style)
}

fn task_label(task: &str) -> String {
    Path::new(task)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(task)
        .to_owned()
}

fn model_label(family: &EvaluationFamilyStatus) -> String {
    family
        .treatment
        .model
        .as_str()
        .strip_prefix("gpt-5.6-")
        .unwrap_or_else(|| family.treatment.model.as_str())
        .to_owned()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use nanocodex::{Model, Thinking};
    use nanocodex_eval::{EvaluationCounts, EvaluationTreatment};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn dashboard_renders_a_representative_live_workset() {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut view = AttachView::new(status(24));
        terminal.draw(|frame| render(frame, &mut view)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(rendered.contains("terminal-bench"));
        assert!(rendered.contains("0/0/1/4"));
        assert!(rendered.contains("read-only SQLite"));
        assert!(view.page_size < view.snapshot.families.len());
    }

    #[test]
    fn dashboard_handles_a_small_terminal_and_scrolls_without_overflow() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut view = AttachView::new(status(24));
        terminal.draw(|frame| render(frame, &mut view)).unwrap();
        let first_page = view.page_size;

        let action = view.handle_event(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        )));
        assert!(matches!(action, AttachAction::Continue { redraw: true }));
        assert_eq!(view.offset, first_page);
        terminal.draw(|frame| render(frame, &mut view)).unwrap();
    }

    fn status(family_count: usize) -> EvaluationStatus {
        EvaluationStatus {
            profile: "terminal-bench".to_owned(),
            digest: "0123456789abcdef".to_owned(),
            tasks: EvaluationCounts {
                unclaimed: i64::try_from(family_count * 4).unwrap(),
                running: i64::try_from(family_count).unwrap(),
                success: 0,
                failed: 0,
            },
            workers: Vec::new(),
            recent_attempts: Default::default(),
            families: (0..family_count)
                .map(|index| EvaluationFamilyStatus {
                    id: format!("family-{index}"),
                    task: format!("/terminal-bench/task-{index}"),
                    treatment: EvaluationTreatment {
                        harness: "nanocodex".to_owned(),
                        model: Model::Luna,
                        thinking: Thinking::High,
                        web_search: false,
                    },
                    desired: 5,
                    unclaimed: 4,
                    running: 1,
                    success: 0,
                    failed: 0,
                })
                .collect(),
        }
    }
}
