//! Persistent project navigation. IDs, never titles, identify selectable threads.
use crate::tui::{format::sanitize_terminal_text_inline, session::SessionSummary};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
pub(super) struct ProjectSidebar {
    pub visible: bool,
    pub focused: bool,
    pub loading: bool,
    pub current_id: Option<String>,
    pub next_refresh: Option<std::time::Instant>,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    pub area: Rect,
    thread_area: Rect,
}

struct Row {
    id: Option<String>,
    label: String,
}

impl ProjectSidebar {
    pub fn load(&mut self, sessions: Vec<SessionSummary>) {
        let selected = self.selected_id().map(str::to_owned);
        let mut groups: BTreeMap<String, Vec<SessionSummary>> = BTreeMap::new();
        for session in sessions {
            let root = session
                .project_root_id
                .as_ref()
                .filter(|id| !id.is_empty())
                .unwrap_or(&session.session_id)
                .clone();
            groups.entry(root).or_default().push(session);
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by_key(|(id, threads)| {
            (
                std::cmp::Reverse(
                    threads
                        .iter()
                        .map(|s| s.updated_at_unix_ms)
                        .max()
                        .unwrap_or(0),
                ),
                id.clone(),
            )
        });
        self.rows.clear();
        for (root, mut threads) in groups {
            threads.sort_by(|a, b| {
                b.updated_at_unix_ms
                    .cmp(&a.updated_at_unix_ms)
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
            let title = threads
                .iter()
                .find_map(|s| s.project_name.as_deref().filter(|name| !name.is_empty()))
                .unwrap_or_else(|| {
                    threads
                        .iter()
                        .find(|s| s.session_id == root)
                        .map(|s| s.preview.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&root)
                });
            self.rows.push(Row {
                id: None,
                label: sanitize_terminal_text_inline(title).into_owned(),
            });
            let mut visited = HashSet::new();
            if threads.iter().any(|s| s.session_id == root) {
                self.append(&root, 1, &threads, &mut visited);
            }
            // Missing/deleted parents and malformed cycles must never hide a thread.
            for thread in &threads {
                self.append(&thread.session_id, 1, &threads, &mut visited);
            }
        }
        self.selected = selected
            .and_then(|id| self.rows.iter().position(|r| r.id.as_ref() == Some(&id)))
            .or_else(|| {
                self.current_id
                    .as_ref()
                    .and_then(|id| self.rows.iter().position(|r| r.id.as_ref() == Some(id)))
            })
            .or_else(|| self.rows.iter().position(|r| r.id.is_some()))
            .unwrap_or(0);
        self.loading = false;
    }
    fn append(
        &mut self,
        id: &str,
        depth: usize,
        threads: &[SessionSummary],
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(id.to_owned()) {
            return;
        }
        let Some(thread) = threads.iter().find(|s| s.session_id == id) else {
            return;
        };
        let title = if thread.preview.is_empty() {
            id
        } else {
            &thread.preview
        };
        self.rows.push(Row {
            id: Some(id.to_owned()),
            label: format!(
                "{}{}",
                "  ".repeat(depth.min(8)),
                format!(
                    "{}{}",
                    match thread.active {
                        Some(true) => "● ",
                        Some(false) => "○ ",
                        None => "? ",
                    },
                    sanitize_terminal_text_inline(title)
                )
            ),
        });
        for child in threads
            .iter()
            .filter(|s| s.parent_agent_id.as_deref() == Some(id))
        {
            self.append(&child.session_id, depth + 1, threads, visited);
        }
    }
    pub fn selected_id(&self) -> Option<&str> {
        self.rows.get(self.selected)?.id.as_deref()
    }
    pub fn step(&mut self, down: bool) {
        if down {
            if let Some(index) =
                ((self.selected + 1)..self.rows.len()).find(|i| self.rows[*i].id.is_some())
            {
                self.selected = index;
            }
        } else if let Some(index) = (0..self.selected)
            .rev()
            .find(|i| self.rows[*i].id.is_some())
        {
            self.selected = index;
        }
    }
    pub fn click(&mut self, y: u16) -> bool {
        if y < self.thread_area.y || y >= self.thread_area.bottom() {
            return false;
        }
        let index = usize::from(y.saturating_sub(self.area.y + 1)) + self.offset;
        if self.rows.get(index).is_some_and(|r| r.id.is_some()) {
            self.selected = index;
            self.focused = true;
            true
        } else {
            false
        }
    }
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, agents: Vec<String>) -> Rect {
        self.area = Rect::default();
        self.thread_area = Rect::default();
        if !self.visible || area.width < 60 || area.height < 6 {
            return area;
        }
        let width = (area.width / 3).clamp(24, 38);
        self.area = Rect { width, ..area };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .title(if self.focused {
                "Projects > Threads •"
            } else {
                "Projects > Threads"
            });
        frame.render_widget(block, self.area);
        let hidden_agents = agents.len().saturating_sub(5);
        let capacity = usize::from(
            area.height
                .saturating_sub(4 + agents.len().min(5) as u16 + u16::from(hidden_agents > 0)),
        );
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + capacity {
            self.offset = self.selected.saturating_sub(capacity.saturating_sub(1));
        }
        let mut lines: Vec<Line<'static>> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(capacity)
            .map(|(i, row)| {
                let style = if i == self.selected {
                    Style::default().add_modifier(if self.focused {
                        Modifier::REVERSED
                    } else {
                        Modifier::BOLD
                    })
                } else {
                    Style::default()
                };
                let label = if row
                    .id
                    .as_ref()
                    .is_some_and(|id| Some(id) == self.current_id.as_ref())
                {
                    format!("› {}", row.label)
                } else {
                    row.label.clone()
                };
                Line::styled(label, style)
            })
            .collect();
        self.thread_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: width - 1,
            height: lines.len() as u16,
        };
        if self.rows.is_empty() {
            lines.push(Line::from(if self.loading {
                "Loading…"
            } else {
                "No threads"
            }));
        }
        if !agents.is_empty() {
            lines.push(Line::from("Running · current thread"));
            lines.extend(agents.into_iter().take(5).map(Line::from));
            if hidden_agents > 0 {
                lines.push(Line::from(format!("  +{hidden_agents} more running")));
            }
        }
        frame.render_widget(
            Paragraph::new(lines),
            Rect {
                x: area.x,
                y: area.y + 1,
                width: width - 1,
                height: area.height - 2,
            },
        );
        frame.render_widget(
            Paragraph::new("F2 toggle · ↑↓ ↵ · r refresh"),
            Rect {
                x: area.x,
                y: area.bottom() - 1,
                width: width - 1,
                height: 1,
            },
        );
        Rect {
            x: area.x + width,
            width: area.width - width,
            ..area
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ReasoningEffort, ReasoningMode};
    fn thread(id: &str, root: &str, parent: Option<&str>) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            project_root_id: Some(root.into()),
            parent_agent_id: parent.map(str::to_owned),
            project_name: None,
            active: None,
            preview: id.into(),
            updated_at_unix_ms: 0,
            model: "sol".into(),
            effort: ReasoningEffort::Medium,
            reasoning_mode: ReasoningMode::Standard,
            workspace: "/work".into(),
        }
    }
    #[test]
    fn nested_threads_and_orphans_are_selectable_once() {
        let mut sidebar = ProjectSidebar::default();
        sidebar.load(vec![
            thread("child", "master", Some("master")),
            thread("master", "master", None),
            thread("grandchild", "master", Some("child")),
            thread("orphan", "master", Some("missing")),
        ]);
        let ids: Vec<_> = sidebar
            .rows
            .iter()
            .filter_map(|r| r.id.as_deref())
            .collect();
        assert_eq!(ids, ["master", "child", "grandchild", "orphan"]);
        sidebar.step(true);
        assert_eq!(sidebar.selected_id(), Some("child"));
        sidebar.step(false);
        assert_eq!(sidebar.selected_id(), Some("master"));
    }
    #[test]
    fn cycles_terminate_and_refresh_preserves_identity() {
        let mut sidebar = ProjectSidebar::default();
        sidebar.load(vec![
            thread("a", "root", Some("b")),
            thread("b", "root", Some("a")),
        ]);
        assert_eq!(sidebar.rows.len(), 3);
        sidebar.step(true);
        sidebar.load(vec![thread("b", "root", None), thread("a", "root", None)]);
        assert_eq!(sidebar.selected_id(), Some("b"));
    }
    #[test]
    fn narrow_layout_keeps_composer_space() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 10)).unwrap();
        let mut sidebar = ProjectSidebar {
            visible: true,
            ..Default::default()
        };
        terminal
            .draw(|frame| {
                let area = frame.area();
                assert_eq!(sidebar.render(frame, area, vec![]), area);
            })
            .unwrap();
        assert_eq!(sidebar.area, Rect::default());
    }
    #[test]
    fn clicks_on_footer_and_activity_never_open_offscreen_threads() {
        let mut sidebar = ProjectSidebar {
            visible: true,
            ..Default::default()
        };
        sidebar.load(
            (0..20)
                .map(|n| thread(&format!("thread-{n}"), "master", None))
                .collect(),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 12)).unwrap();
        terminal
            .draw(|frame| {
                sidebar.render(frame, frame.area(), vec!["worker".into()]);
            })
            .unwrap();
        assert!(!sidebar.click(0));
        assert!(!sidebar.click(sidebar.thread_area.bottom()));
        assert!(!sidebar.click(11));
    }

    #[test]
    fn current_thread_is_initial_selection_and_project_names_are_sanitized() {
        let mut sidebar = ProjectSidebar {
            current_id: Some("b".into()),
            ..Default::default()
        };
        let mut b = thread("b", "b", None);
        b.project_name = Some("Named project".into());
        sidebar.load(vec![thread("a", "a", None), b]);
        assert_eq!(sidebar.selected_id(), Some("b"));
        assert!(sidebar.rows.iter().any(|row| row.label == "Named project"));
    }
}

/// The same reducer consumes retained history and live managed records.
#[derive(Default)]
pub(super) struct ManagedActivity {
    agents: BTreeMap<u64, ManagedAgent>,
}
#[derive(Default)]
struct ManagedAgent {
    role: String,
    parent: Option<u64>,
    active: bool,
    lifecycle_seen: bool,
}
impl ManagedActivity {
    pub fn observe(&mut self, record: &crate::tui::transcript::TranscriptRecord) {
        if let Some(id) = record.managed_agent_id() {
            match record.kind() {
                "run.started" | "run.completed" | "run.failed" => {
                    let agent = self.agents.entry(id).or_default();
                    agent.active = record.kind() == "run.started";
                    agent.lifecycle_seen = true;
                }
                _ => {}
            }
        }
        if record.kind() != "tool.result" {
            return;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(record.payload_json()) else {
            return;
        };
        let tool = payload["tool"].as_str().unwrap_or_default();
        if !matches!(
            tool,
            "spawn_agent" | "list_agents" | "wait_agent" | "interrupt_agent" | "close_agent"
        ) {
            return;
        }
        fn normalize(value: &serde_json::Value) -> serde_json::Value {
            value
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| value.clone())
        }
        let structured = normalize(&payload["structured_result"]);
        let result = if structured.is_null()
            || structured.as_object().is_some_and(|o| o.is_empty())
            || structured.as_array().is_some_and(|a| a.is_empty())
            || structured.as_str().is_some_and(|s| s.is_empty())
        {
            normalize(&payload["result"])
        } else {
            structured
        };
        if let Some(agents) = result["agents"].as_array() {
            for agent in agents {
                self.merge(agent, false, record.managed_agent_id());
            }
        } else {
            self.merge(&result, tool == "spawn_agent", record.managed_agent_id());
        }
    }
    fn merge(&mut self, value: &serde_json::Value, initial: bool, parent: Option<u64>) {
        let Some(id) = value["agent_id"].as_u64() else {
            return;
        };
        let Some(status) = value["status"]["state"].as_str() else {
            return;
        };
        let agent = self.agents.entry(id).or_default();
        if let Some(role) = value["role"].as_str() {
            agent.role = sanitize_terminal_text_inline(role).into_owned();
        }
        if value.get("parent_agent_id").is_some() {
            agent.parent = value["parent_agent_id"].as_u64();
        }
        if initial && agent.parent.is_none() {
            agent.parent = parent;
        }
        // A spawn receipt describes admission, and may arrive after child completion.
        if !initial || !agent.lifecycle_seen {
            agent.active = matches!(status, "pending" | "running" | "closing");
        }
    }
    pub fn labels(&self) -> Vec<String> {
        self.agents
            .iter()
            .filter(|(_, agent)| agent.active)
            .map(|(id, agent)| {
                let parent = agent
                    .parent
                    .map(|id| format!(" ↳#{id}"))
                    .unwrap_or_default();
                format!("  ● #{id} {}{parent}", agent.role)
            })
            .collect()
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;
    use serde_json::json;
    fn record(
        kind: &str,
        child: Option<u64>,
        payload: serde_json::Value,
    ) -> crate::tui::transcript::TranscriptRecord {
        serde_json::from_value(json!({"schema_version":1,"sequence":1,"recorded_at_unix_ms":0,"source":"agent","type":kind,"agent":{"protocol_version":1,"request_id":"test","sequence":1,"managed_agent_id":child},"payload":payload})).unwrap()
    }
    #[test]
    fn lifecycle_and_partial_tool_snapshots_share_one_reducer() {
        let mut activity = ManagedActivity::default();
        activity.observe(&record("tool.result", None, json!({"tool":"spawn_agent","result":"{\"agent_id\":1,\"role\":\"reviewer\",\"status\":{\"state\":\"running\"}}"})));
        activity.observe(&record("run.started", Some(2), json!({})));
        assert_eq!(activity.labels().len(), 2);
        activity.observe(&record("tool.result", None, json!({"tool":"wait_agent","structured_result":{"agents":[{"agent_id":1,"status":{"state":"completed"}}]}})));
        assert_eq!(activity.labels().len(), 1);
        activity.observe(&record("run.completed", Some(2), json!({})));
        assert!(activity.labels().is_empty());
    }
    #[test]
    fn late_spawn_receipt_cannot_revive_a_completed_child() {
        let mut activity = ManagedActivity::default();
        activity.observe(&record("run.completed", Some(7), json!({})));
        activity.observe(&record("tool.result", Some(2), json!({"tool":"spawn_agent","result":{"agent_id":7,"role":"review","status":{"state":"running"}}})));
        assert!(activity.labels().is_empty());
        assert_eq!(activity.agents[&7].parent, Some(2));
        activity.observe(&record("run.started", Some(7), json!({})));
        assert_eq!(activity.labels().len(), 1);
    }
}
