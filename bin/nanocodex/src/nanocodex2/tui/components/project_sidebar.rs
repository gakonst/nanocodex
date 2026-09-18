//! Workspace navigation: stable identities, cached hierarchy, and local filtering.
use crate::tui::{format::sanitize_terminal_text_inline, session::SessionSummary, theme::Theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Default)]
pub(super) struct ProjectSidebar {
    pub visible: bool,
    pub focused: bool,
    pub loading: bool,
    pub current_id: Option<String>,
    pub next_refresh: Option<std::time::Instant>,
    nodes: Vec<Row>,
    rows: Vec<usize>,
    expanded: HashSet<String>,
    last_current: Option<String>,
    pub filtering: bool,
    pub status_generation: u64,
    pub catalog_generation: Option<u64>,
    statuses: HashMap<String, bool>,
    filter: String,
    selected: usize,
    offset: usize,
    pub area: Rect,
    thread_area: Rect,
    rendered_hits: Vec<Option<usize>>,
}
struct Row {
    id: String,
    project: String,
    label: String,
    depth: usize,
    parent: Option<usize>,
    children: usize,
    active: usize,
}
fn human_title(value: &str, id: &str, fallback: &str) -> String {
    let value = sanitize_terminal_text_inline(value);
    let value = value.trim();
    let uuid = value.len() == 36 && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if value.is_empty() || value == id || uuid {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}
impl ProjectSidebar {
    pub fn load(&mut self, sessions: Vec<SessionSummary>) {
        let selected = self.selected_id().map(str::to_owned);
        let mut groups: BTreeMap<String, Vec<SessionSummary>> = BTreeMap::new();
        self.statuses.clear();
        let mut unique = HashSet::new();
        for session in sessions {
            if !unique.insert(session.session_id.clone()) {
                continue;
            }
            if let Some(active) = session.active {
                self.statuses.insert(session.session_id.clone(), active);
            }
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
        self.nodes.clear();
        for (root, mut threads) in groups {
            threads.sort_by(|a, b| {
                b.updated_at_unix_ms
                    .cmp(&a.updated_at_unix_ms)
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
            threads.dedup_by(|a, b| a.session_id == b.session_id);
            let master = threads.iter().position(|s| s.session_id == root);
            let title = threads
                .iter()
                .find_map(|s| s.project_name.as_deref().filter(|s| !s.trim().is_empty()))
                .or_else(|| master.map(|i| threads[i].preview.as_str()))
                .unwrap_or("");
            // A missing master must not manufacture a resumable ID.
            let target = master.unwrap_or(0);
            let project_index = self.nodes.len();
            self.nodes.push(Row {
                id: threads[target].session_id.clone(),
                project: root.clone(),
                label: human_title(title, &root, "Untitled project"),
                depth: 0,
                parent: None,
                children: threads.len().saturating_sub(1),
                active: threads.iter().filter(|s| s.active == Some(true)).count(),
            });
            if self.last_current != self.current_id
                && threads
                    .iter()
                    .any(|s| Some(&s.session_id) == self.current_id.as_ref())
            {
                self.expanded.insert(root.clone());
            }
            let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
            for (i, thread) in threads.iter().enumerate() {
                if let Some(parent) = thread.parent_agent_id.as_deref() {
                    children.entry(parent).or_default().push(i);
                }
            }
            let mut visited = HashSet::new();
            visited.insert(target);
            let mut stack = Vec::new();
            if let Some(indices) = children.get(threads[target].session_id.as_str()) {
                stack.extend(indices.iter().rev().map(|i| (*i, 1, project_index)));
            }
            // Iterative traversal handles deep trees and cycles without recursion.
            for seed in std::iter::once(target).chain(0..threads.len()) {
                if stack.is_empty() && !visited.contains(&seed) {
                    stack.push((seed, 1, project_index));
                }
                while let Some((i, depth, parent)) = stack.pop() {
                    if !visited.insert(i) {
                        continue;
                    }
                    let thread = &threads[i];
                    let index = self.nodes.len();
                    self.nodes.push(Row {
                        id: thread.session_id.clone(),
                        project: root.clone(),
                        label: human_title(&thread.preview, &thread.session_id, "Untitled thread"),
                        depth,
                        parent: Some(parent),
                        children: 0,
                        active: usize::from(thread.active == Some(true)),
                    });
                    if let Some(indices) = children.get(thread.session_id.as_str()) {
                        stack.extend(indices.iter().rev().map(|i| (*i, depth + 1, index)));
                    }
                }
            }
        }
        self.update_statuses(Vec::new());
        let preferred = if self.last_current != self.current_id {
            self.current_id.clone()
        } else {
            selected
        };
        self.last_current = self.current_id.clone();
        self.rebuild(preferred.as_deref());
        self.loading = false;
    }
    fn rebuild(&mut self, selected: Option<&str>) {
        let query = self.filter.to_lowercase();
        let mut matches = HashSet::new();
        let mut matching_projects = HashSet::new();
        if !query.is_empty() {
            for (i, row) in self.nodes.iter().enumerate() {
                if row.label.to_lowercase().contains(&query) {
                    if row.depth == 0 {
                        matching_projects.insert(row.project.as_str());
                    }
                    matches.insert(i);
                    let mut parent = row.parent;
                    while let Some(p) = parent {
                        if !matches.insert(p) {
                            break;
                        }
                        parent = self.nodes[p].parent;
                    }
                }
            }
        }
        self.rows = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, row)| {
                if query.is_empty() {
                    row.depth == 0 || self.expanded.contains(&row.project)
                } else {
                    matches.contains(i) || matching_projects.contains(row.project.as_str())
                }
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = selected
            .and_then(|id| self.rows.iter().position(|i| self.nodes[*i].id == id))
            .or_else(|| {
                selected
                    .and_then(|id| self.nodes.iter().find(|n| n.id == id))
                    .and_then(|node| {
                        self.rows.iter().position(|i| {
                            self.nodes[*i].project == node.project && self.nodes[*i].depth == 0
                        })
                    })
            })
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
        self.offset = self.offset.min(self.rows.len().saturating_sub(1));
    }
    pub fn selected_id(&self) -> Option<&str> {
        Some(&self.nodes[*self.rows.get(self.selected)?].id)
    }
    pub fn relevant_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(id) = self.selected_id() {
            ids.push(id.to_owned());
        }
        for i in self.rows.iter().skip(self.offset).take(12) {
            if ids.len() == 12 {
                break;
            }
            let id = &self.nodes[*i].id;
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }
    pub fn update_statuses(&mut self, statuses: Vec<(String, bool)>) {
        self.statuses.extend(statuses);
        let mut totals: HashMap<String, usize> = HashMap::new();
        for row in &mut self.nodes {
            if let Some(&active) = self.statuses.get(&row.id) {
                row.active = usize::from(active);
            }
            *totals.entry(row.project.clone()).or_default() +=
                usize::from(self.statuses.get(&row.id).copied().unwrap_or(false));
        }
        for row in &mut self.nodes {
            if row.depth == 0 {
                row.active = totals.get(&row.project).copied().unwrap_or(0);
            }
        }
    }
    pub fn step(&mut self, down: bool) {
        self.selected = if down {
            (self.selected + 1).min(self.rows.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }
    pub fn expand(&mut self, open: bool) {
        let Some(&index) = self.rows.get(self.selected) else {
            return;
        };
        let project = self.nodes[index].project.clone();
        let root = self
            .nodes
            .iter()
            .find(|n| n.project == project && n.depth == 0)
            .map(|n| n.id.clone());
        if open {
            self.expanded.insert(project);
        } else {
            self.expanded.remove(&project);
        }
        let selected = if open {
            self.selected_id().map(str::to_owned)
        } else {
            root
        };
        self.rebuild(selected.as_deref());
    }
    pub fn filter_input(&mut self, ch: Option<char>) {
        let selected = self.selected_id().map(str::to_owned);
        if let Some(ch) = ch {
            self.filter.push(ch);
        } else {
            self.filter.pop();
        }
        self.rebuild(selected.as_deref());
    }
    pub fn clear_filter(&mut self) {
        let selected = self.selected_id().map(str::to_owned);
        self.filter.clear();
        self.filtering = false;
        self.rebuild(selected.as_deref());
    }
    pub fn click(&mut self, x: u16, y: u16) -> bool {
        if y == self.area.y + 1 {
            self.filtering = true;
            self.focused = true;
            return false;
        }
        if y < self.thread_area.y || y >= self.thread_area.bottom() {
            return false;
        }
        let Some(Some(index)) = self
            .rendered_hits
            .get(usize::from(y - self.thread_area.y))
            .copied()
        else {
            return false;
        };
        let Some(&node) = self.rows.get(index) else {
            return false;
        };
        self.selected = index;
        self.focused = true;
        if x <= self.area.x + 3 && self.nodes[node].depth == 0 && self.nodes[node].children > 0 {
            let open = !self.expanded.contains(&self.nodes[node].project);
            self.expand(open);
            return false;
        }
        true
    }
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        agents: Vec<String>,
    ) -> Rect {
        self.area = Rect::default();
        self.thread_area = Rect::default();
        self.rendered_hits.clear();
        if !self.visible || area.width < 60 || area.height < 6 {
            return area;
        }
        let width = (area.width / 3).clamp(26, 38);
        self.area = Rect { width, ..area };
        let muted = Style::default().fg(theme.muted());
        let accent = Style::default().fg(theme.accent());
        frame.render_widget(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(theme.border())),
            self.area,
        );
        let content = Rect {
            x: area.x + 1,
            y: area.y,
            width: width - 3,
            height: 1,
        };
        let count = self.nodes.iter().filter(|n| n.depth == 0).count();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "PROJECTS",
                    if self.focused { accent } else { muted }.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {count}{}", if self.loading { " · syncing" } else { "" }),
                    muted,
                ),
            ])),
            content,
        );
        let search = if self.filtering || !self.filter.is_empty() {
            format!("/ {}{}", self.filter, if self.filtering { "▏" } else { "" })
        } else {
            "/ find a thread".into()
        };
        frame.render_widget(
            Paragraph::new(search).style(if self.filtering { accent } else { muted }),
            Rect {
                y: area.y + 1,
                ..content
            },
        );
        let capacity = usize::from(area.height.saturating_sub(4));
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        let navigation_capacity = capacity.saturating_sub(agents.len().min(3)).max(1);
        if self.selected >= self.offset + navigation_capacity {
            self.offset = self
                .selected
                .saturating_sub(navigation_capacity.saturating_sub(1));
        }
        self.thread_area = Rect {
            x: area.x,
            y: area.y + 3,
            width: width - 1,
            height: capacity as u16,
        };
        let current_project = self
            .nodes
            .iter()
            .find(|n| Some(&n.id) == self.current_id.as_ref())
            .map(|n| n.project.as_str());
        let mut line = 0;
        for (position, &index) in self
            .rows
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(capacity)
        {
            if line >= capacity {
                break;
            }
            self.rendered_hits.push(Some(position));
            let row = &self.nodes[index];
            let selected = position == self.selected;
            let current = Some(&row.id) == self.current_id.as_ref();
            let style = if selected {
                Style::default()
                    .bg(theme.code_background())
                    .fg(if self.focused {
                        theme.accent()
                    } else {
                        theme.text()
                    })
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(if row.depth == 0 {
                    theme.text()
                } else {
                    theme.muted()
                })
            };
            let disclosure = if row.depth == 0 {
                if row.children == 0 {
                    "· "
                } else if self.expanded.contains(&row.project) || !self.filter.is_empty() {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "└ "
            };
            let local = if row.depth == 0 && current_project == Some(row.project.as_str()) {
                agents.len()
            } else {
                0
            };
            let running = row.active + local;
            let badge = if running > 0 {
                format!(" ●{running}")
            } else if row.depth == 0 && row.children > 0 {
                format!(" {}", row.children)
            } else {
                String::new()
            };
            let prefix = format!(
                "{}{}{}",
                if current {
                    "›"
                } else if selected && self.focused {
                    "▎"
                } else {
                    " "
                },
                "  ".repeat(row.depth.min(5)),
                disclosure
            );
            let available = usize::from(width.saturating_sub(2))
                .saturating_sub(prefix.chars().count() + badge.chars().count());
            let label = truncate(&row.label, available);
            let spans = vec![
                Span::styled(prefix, if current { accent } else { style }),
                Span::styled(label, style),
                Span::styled(badge, if running > 0 { accent } else { muted }),
            ];
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(style),
                Rect {
                    y: self.thread_area.y + line as u16,
                    height: 1,
                    ..self.thread_area
                },
            );
            line += 1;
            if current {
                for agent in agents.iter().take(3) {
                    if line >= capacity {
                        break;
                    }
                    let label = format!("{}{}", "  ".repeat((row.depth + 1).min(5)), agent.trim());
                    frame.render_widget(
                        Paragraph::new(truncate(&label, usize::from(width - 2))).style(muted),
                        Rect {
                            y: self.thread_area.y + line as u16,
                            height: 1,
                            ..self.thread_area
                        },
                    );
                    self.rendered_hits.push(None);
                    line += 1;
                }
            }
        }
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.loading {
                    "  Loading projects…"
                } else if !self.filter.is_empty() {
                    "  No matching threads"
                } else {
                    "  No projects yet"
                })
                .style(muted),
                self.thread_area,
            );
        }
        let footer = if self.filtering {
            "esc clear · enter done"
        } else if self.focused {
            "←→ fold · ↵ open · tab chat"
        } else {
            "F2 projects"
        };
        frame.render_widget(
            Paragraph::new(footer).style(muted),
            Rect {
                y: area.bottom() - 1,
                ..content
            },
        );
        Rect {
            x: area.x + width,
            width: area.width - width,
            ..area
        }
    }
}
fn truncate(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut result = String::new();
    for ch in value.chars() {
        let next = ch.width().unwrap_or(0);
        if used + next > width - 1 {
            break;
        }
        result.push(ch);
        used += next;
    }
    result.push('…');
    result
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;
    use crate::config::{ReasoningEffort, ReasoningMode};
    fn thread(id: &str, root: &str, parent: Option<&str>) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            project_root_id: Some(root.into()),
            parent_agent_id: parent.map(str::to_owned),
            project_name: Some(format!("Project {root}")),
            active: None,
            preview: format!("Task {id}"),
            updated_at_unix_ms: 0,
            model: "sol".into(),
            effort: ReasoningEffort::Medium,
            reasoning_mode: ReasoningMode::Standard,
            workspace: "/work".into(),
        }
    }
    fn ids(sidebar: &ProjectSidebar) -> Vec<&str> {
        sidebar
            .rows
            .iter()
            .map(|i| sidebar.nodes[*i].id.as_str())
            .collect()
    }
    #[test]
    fn synthetic_sidebar_preview() {
        let mut sidebar = ProjectSidebar {
            visible: true,
            focused: true,
            current_id: Some("review".into()),
            ..Default::default()
        };
        let mut data = Vec::new();
        for (id, name) in [
            ("atlas", "Atlas"),
            ("beacon", "Beacon"),
            ("canvas", "Canvas"),
            ("docs", "Documentation"),
            ("infra", "Infrastructure"),
            ("research", "Research"),
        ] {
            let mut root = thread(id, id, None);
            root.project_name = Some(name.into());
            data.push(root);
        }
        for (id, title, parent) in [
            ("review", "Review authentication", "atlas"),
            ("tests", "Exercise recovery paths", "review"),
            ("design", "Navigation polish", "atlas"),
        ] {
            let mut child = thread(id, "atlas", Some(parent));
            child.preview = title.into();
            child.project_name = Some("Atlas".into());
            data.push(child);
        }
        sidebar.load(data);
        sidebar.update_statuses(vec![("review".into(), true)]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 22)).unwrap();
        terminal
            .draw(|frame| {
                sidebar.render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                    vec![
                        "● reviewer · running".into(),
                        "● test runner · running".into(),
                    ],
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let snapshot = (0..22)
            .map(|y| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        println!("{snapshot}");
        if let Ok(path) = std::env::var("NANOCODEX_SIDEBAR_PREVIEW") {
            let cells: Vec<_> = (0..22)
                .flat_map(|y| {
                    (0..40).map(move |x| {
                        let cell = &buffer[(x, y)];
                        serde_json::json!({"x": x, "y": y, "text": cell.symbol(),
                    "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                    "bold": cell.modifier.contains(Modifier::BOLD)})
                    })
                })
                .collect();
            std::fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({"width":40,"height":22,"cells":cells}))
                    .unwrap(),
            )
            .unwrap();
        }
        assert!(snapshot.contains("PROJECTS"));
        assert!(snapshot.contains("reviewer"));
        assert!(snapshot.contains("Review authentication"));
        assert!(!snapshot.contains("Untitled"));
    }
    #[test]
    fn one_project_row_and_only_current_project_expands() {
        let mut sidebar = ProjectSidebar {
            current_id: Some("child".into()),
            ..Default::default()
        };
        sidebar.load(vec![
            thread("root", "root", None),
            thread("child", "root", Some("root")),
            thread("other", "other", None),
            thread("hidden", "other", Some("other")),
        ]);
        assert_eq!(ids(&sidebar), ["other", "root", "child"]);
        assert_eq!(sidebar.selected_id(), Some("child"));
        sidebar.expand(false);
        assert_eq!(sidebar.selected_id(), Some("root"));
        assert_eq!(ids(&sidebar), ["other", "root"]);
        sidebar.expand(true);
        assert_eq!(ids(&sidebar), ["other", "root", "child"]);
    }
    #[test]
    fn filtering_reveals_children_and_ancestors_without_changing_folds() {
        let mut sidebar = ProjectSidebar::default();
        sidebar.load(vec![
            thread("root", "root", None),
            thread("review", "root", Some("root")),
            thread("needle", "root", Some("review")),
            thread("other", "root", None),
        ]);
        assert_eq!(ids(&sidebar), ["root"]);
        for ch in "needle".chars() {
            sidebar.filter_input(Some(ch));
        }
        assert_eq!(ids(&sidebar), ["root", "review", "needle"]);
        sidebar.clear_filter();
        assert_eq!(ids(&sidebar), ["root"]);
    }
    #[test]
    fn malformed_hierarchy_keeps_every_identity_once() {
        let mut sidebar = ProjectSidebar::default();
        sidebar.load(vec![
            thread("a", "root", Some("b")),
            thread("b", "root", Some("a")),
            thread("orphan", "root", Some("missing")),
            thread("b", "root", Some("a")),
        ]);
        sidebar.expand(true);
        let mut got = ids(&sidebar);
        got.sort();
        assert_eq!(got, ["a", "b", "orphan"]);
        assert!(!got.contains(&"root"));
    }
    #[test]
    fn refresh_preserves_selection_and_collapsed_state() {
        let mut sidebar = ProjectSidebar::default();
        let data = || {
            vec![
                thread("a", "a", None),
                thread("child", "a", Some("a")),
                thread("b", "b", None),
            ]
        };
        sidebar.load(data());
        sidebar.step(true);
        sidebar.load(data());
        assert_eq!(sidebar.selected_id(), Some("b"));
        assert_eq!(ids(&sidebar), ["a", "b"]);
    }
    #[test]
    fn clearing_search_returns_to_selected_project_and_refresh_expires_status() {
        let mut sidebar = ProjectSidebar::default();
        let data = || {
            vec![
                thread("a", "a", None),
                thread("b", "b", None),
                thread("needle", "b", Some("b")),
            ]
        };
        sidebar.load(data());
        for ch in "needle".chars() {
            sidebar.filter_input(Some(ch));
        }
        sidebar.step(true);
        assert_eq!(sidebar.selected_id(), Some("needle"));
        sidebar.clear_filter();
        assert_eq!(sidebar.selected_id(), Some("b"));
        sidebar.update_statuses(vec![("needle".into(), true)]);
        sidebar.load(data());
        assert!(sidebar.nodes.iter().all(|n| n.active == 0));
    }
    #[test]
    fn status_rollup_does_not_double_count_master() {
        let mut sidebar = ProjectSidebar::default();
        sidebar.load(vec![
            thread("root", "root", None),
            thread("child", "root", Some("root")),
        ]);
        sidebar.update_statuses(vec![("root".into(), true), ("child".into(), true)]);
        assert_eq!(sidebar.nodes[0].active, 2);
        sidebar.update_statuses(vec![("child".into(), false)]);
        assert_eq!(sidebar.nodes[0].active, 1);
    }
    #[test]
    fn no_uuid_fallback_and_unicode_labels_fit_cells() {
        let uuid = "00000000-0000-0000-0000-000000000000";
        assert_eq!(
            human_title(uuid, uuid, "Untitled thread"),
            "Untitled thread"
        );
        assert_eq!(truncate("日本語 project", 6), "日本…");
        assert!(truncate("日本語 project", 6).width() <= 6);
    }
    #[test]
    fn footer_clicks_do_not_open_threads_and_narrow_layout_hides_rail() {
        let mut sidebar = ProjectSidebar {
            visible: true,
            focused: true,
            ..Default::default()
        };
        sidebar.load(vec![
            thread("root", "root", None),
            thread("child", "root", Some("root")),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 12)).unwrap();
        terminal
            .draw(|frame| {
                sidebar.render(frame, frame.area(), &Theme::default(), vec![]);
            })
            .unwrap();
        assert!(!sidebar.click(5, 0));
        assert!(!sidebar.click(5, 11));
        assert!(!sidebar.click(2, 3));
        assert_eq!(ids(&sidebar), ["root", "child"]);
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 50, 10);
                assert_eq!(sidebar.render(frame, area, &Theme::default(), vec![]), area);
            })
            .unwrap();
        assert_eq!(sidebar.area, Rect::default());
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
