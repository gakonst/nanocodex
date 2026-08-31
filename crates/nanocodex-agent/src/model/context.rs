use std::sync::Arc;

use chrono::{Local, Utc};
use nanocodex_oai_api::responses::{ContentItem, MessageRole, ResponseItem};

use crate::agent::ExecutionEnvironment;
pub(crate) use crate::session::{
    AgentsMdSnapshot, ContextBaseline, ContextSnapshot, EnvironmentSnapshot,
};

const REPLACEMENT_NOTICE: &str =
    "These AGENTS.md instructions replace all previously provided AGENTS.md instructions.";
const REMOVAL_NOTICE: &str = "The previously provided AGENTS.md instructions no longer apply.";

impl ContextBaseline {
    pub(crate) fn reconstruct(history: &[ResponseItem]) -> Self {
        let reconstructed = ContextSnapshot::reconstruct(history);
        if reconstructed.is_empty() {
            Self::Missing
        } else {
            Self::Known(reconstructed)
        }
    }
}

#[derive(Clone)]
pub(crate) struct ContextState {
    selected_agents_md: Option<Arc<str>>,
    visible: Option<ContextSnapshot>,
}

pub(crate) struct ContextUpdate {
    pub(crate) item: ResponseItem,
    pub(crate) full: bool,
}

impl ContextState {
    pub(crate) fn new(selected_agents_md: Option<Arc<str>>, baseline: ContextBaseline) -> Self {
        Self {
            selected_agents_md,
            visible: match baseline {
                ContextBaseline::Missing => None,
                ContextBaseline::Known(snapshot) => Some(snapshot),
            },
        }
    }

    pub(crate) fn capture(
        &self,
        cwd: &str,
        shell: &str,
        execution_environment: Option<&ExecutionEnvironment>,
    ) -> ContextSnapshot {
        ContextSnapshot::capture(
            cwd,
            shell,
            self.selected_agents_md.as_deref(),
            execution_environment,
        )
    }

    pub(crate) fn update(&mut self, current: ContextSnapshot) -> Option<ContextUpdate> {
        let full = self.visible.is_none();
        let item = self.visible.as_ref().map_or_else(
            || Some(current.full_item()),
            |previous| current.diff_item(previous),
        );
        self.visible = Some(current);
        item.map(|item| ContextUpdate { item, full })
    }

    pub(crate) fn establish(&mut self, current: ContextSnapshot) {
        self.visible = Some(current);
    }

    pub(crate) fn require_full_reinjection(&mut self) {
        self.visible = None;
    }

    pub(crate) const fn snapshot(&self) -> Option<&ContextSnapshot> {
        self.visible.as_ref()
    }

    pub(crate) fn baseline(&self) -> ContextBaseline {
        self.visible
            .as_ref()
            .map_or(ContextBaseline::Missing, |snapshot| {
                ContextBaseline::Known(snapshot.clone())
            })
    }
}

impl ContextSnapshot {
    pub(crate) fn capture(
        cwd: &str,
        shell: &str,
        agents_md: Option<&str>,
        execution_environment: Option<&ExecutionEnvironment>,
    ) -> Self {
        let detected;
        let local_time = if let Some(configured) = execution_environment {
            configured
        } else {
            detected = detected_execution_environment();
            &detected
        };
        Self::capture_at(
            cwd,
            shell,
            agents_md,
            &local_time.current_date,
            &local_time.timezone,
        )
    }

    pub(super) fn capture_at(
        cwd: &str,
        shell: &str,
        agents_md: Option<&str>,
        current_date: &str,
        timezone: &str,
    ) -> Self {
        Self {
            agents_md: agents_md.map(|text| AgentsMdSnapshot {
                directory: cwd.to_owned(),
                text: text.to_owned(),
            }),
            environment: Some(EnvironmentSnapshot {
                cwd: cwd.to_owned(),
                shell: shell.to_owned(),
                current_date: current_date.to_owned(),
                timezone: timezone.to_owned(),
            }),
        }
    }

    pub(crate) fn reconstruct(history: &[ResponseItem]) -> Self {
        let mut agents_md = None;
        let mut environment = None;
        for text in contextual_text(history) {
            if text.starts_with("# AGENTS.md instructions") {
                agents_md = parse_agents_md(text);
            }
            if text.starts_with("<environment_context>") {
                apply_environment_fragment(&mut environment, text);
            }
        }
        Self {
            agents_md,
            environment,
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.agents_md.is_none() && self.environment.is_none()
    }

    pub(crate) fn full_item(&self) -> ResponseItem {
        let mut content = Vec::with_capacity(2);
        let mut kinds = Vec::with_capacity(2);
        if let Some(agents_md) = &self.agents_md {
            content.push(ContentItem::InputText {
                text: render_agents_md(agents_md, &agents_md.text).into_boxed_str(),
            });
            kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                "agents_md.instructions".into(),
            ));
        }
        if let Some(environment) = &self.environment {
            content.push(ContentItem::InputText {
                text: render_environment(environment, /*full*/ true).into_boxed_str(),
            });
            kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                "environments.environment_context".into(),
            ));
        }
        let mut item = ResponseItem::message(MessageRole::User, content);
        item.set_message_content_item_kinds(kinds);
        item
    }

    fn diff_item(&self, previous: &Self) -> Option<ResponseItem> {
        let mut content = Vec::with_capacity(2);
        let mut kinds = Vec::with_capacity(2);
        match (&previous.agents_md, &self.agents_md) {
            (before, after) if before == after => {}
            (Some(_), Some(after)) => {
                content.push(ContentItem::InputText {
                    text: render_agents_md(
                        after,
                        &format!("{REPLACEMENT_NOTICE}\n\n{}", after.text),
                    )
                    .into_boxed_str(),
                });
                kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                    "agents_md.instructions".into(),
                ));
            }
            (Some(_), None) => {
                content.push(ContentItem::InputText {
                    text: render_agents_md_removal().into_boxed_str(),
                });
                kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                    "agents_md.instructions".into(),
                ));
            }
            (None, Some(after)) => {
                content.push(ContentItem::InputText {
                    text: render_agents_md(after, &after.text).into_boxed_str(),
                });
                kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                    "agents_md.instructions".into(),
                ));
            }
            (None, None) => {}
        }
        match (&previous.environment, &self.environment) {
            (before, after) if before == after => {}
            (Some(before), Some(after)) => {
                if let Some(rendered) = render_environment_diff(before, after) {
                    content.push(ContentItem::InputText {
                        text: rendered.into_boxed_str(),
                    });
                    kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                        "environments.environment_context".into(),
                    ));
                }
            }
            (_, Some(after)) => {
                content.push(ContentItem::InputText {
                    text: render_environment(after, /*full*/ true).into_boxed_str(),
                });
                kinds.push(nanocodex_oai_api::responses::ContentItemKind(
                    "environments.environment_context".into(),
                ));
            }
            (Some(_), None) | (None, None) => {}
        }
        (!content.is_empty()).then(|| {
            let mut item = ResponseItem::message(MessageRole::User, content);
            item.set_message_content_item_kinds(kinds);
            item
        })
    }
}

fn contextual_text(history: &[ResponseItem]) -> impl Iterator<Item = &str> {
    history.iter().flat_map(|item| match item {
        ResponseItem::Message {
            role: MessageRole::User,
            content,
            ..
        } => content
            .iter()
            .filter_map(|content| match content {
                ContentItem::InputText { text } => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    })
}

fn parse_agents_md(text: &str) -> Option<AgentsMdSnapshot> {
    if text.contains(REMOVAL_NOTICE) {
        return None;
    }
    let instructions = between(text, "<INSTRUCTIONS>\n", "\n</INSTRUCTIONS>")?;
    let instructions = instructions
        .strip_prefix(REPLACEMENT_NOTICE)
        .and_then(|instructions| instructions.strip_prefix("\n\n"))
        .unwrap_or(instructions);
    let directory = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# AGENTS.md instructions for "))
        .unwrap_or_default()
        .to_owned();
    Some(AgentsMdSnapshot {
        directory,
        text: instructions.to_owned(),
    })
}

fn apply_environment_fragment(current: &mut Option<EnvironmentSnapshot>, fragment: &str) {
    let cwd = xml_value(fragment, "cwd");
    let shell = xml_value(fragment, "shell");
    let current_date = xml_value(fragment, "current_date");
    let timezone = xml_value(fragment, "timezone");
    match current {
        Some(current) => {
            if let Some(cwd) = cwd {
                current.cwd = cwd;
            }
            if let Some(shell) = shell {
                current.shell = shell;
            }
            if let Some(current_date) = current_date {
                current.current_date = current_date;
            }
            if let Some(timezone) = timezone {
                current.timezone = timezone;
            }
        }
        None => {
            *current = Some(EnvironmentSnapshot {
                cwd: cwd.unwrap_or_default(),
                shell: shell.unwrap_or_default(),
                current_date: current_date.unwrap_or_default(),
                timezone: timezone.unwrap_or_default(),
            });
        }
    }
}

fn render_agents_md(snapshot: &AgentsMdSnapshot, text: &str) -> String {
    format!(
        "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n{text}\n</INSTRUCTIONS>",
        snapshot.directory
    )
}

fn render_agents_md_removal() -> String {
    format!("# AGENTS.md instructions\n\n<INSTRUCTIONS>\n{REMOVAL_NOTICE}\n</INSTRUCTIONS>")
}

fn render_environment(snapshot: &EnvironmentSnapshot, full: bool) -> String {
    let mut output = String::from("<environment_context>\n");
    push_xml_line(&mut output, "cwd", &snapshot.cwd);
    push_xml_line(&mut output, "shell", &snapshot.shell);
    push_xml_line(&mut output, "current_date", &snapshot.current_date);
    push_xml_line(&mut output, "timezone", &snapshot.timezone);
    if full {
        push_filesystem(&mut output, &snapshot.cwd);
    }
    output.push_str("</environment_context>");
    output
}

fn render_environment_diff(
    before: &EnvironmentSnapshot,
    after: &EnvironmentSnapshot,
) -> Option<String> {
    let mut output = String::from("<environment_context>\n");
    let initial_len = output.len();
    if before.cwd != after.cwd {
        push_xml_line(&mut output, "cwd", &after.cwd);
    }
    if before.shell != after.shell {
        push_xml_line(&mut output, "shell", &after.shell);
    }
    let turn_context_changed =
        before.current_date != after.current_date || before.timezone != after.timezone;
    if turn_context_changed {
        push_xml_line(&mut output, "current_date", &after.current_date);
        push_xml_line(&mut output, "timezone", &after.timezone);
        push_filesystem(&mut output, &after.cwd);
    }
    if output.len() == initial_len {
        return None;
    }
    output.push_str("</environment_context>");
    Some(output)
}

fn push_filesystem(output: &mut String, cwd: &str) {
    output.push_str("  <filesystem><workspace_roots><root>");
    push_xml_escaped_text(output, cwd);
    output.push_str(
        "</root></workspace_roots><permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile></filesystem>\n",
    );
}

fn push_xml_line(output: &mut String, tag: &str, value: &str) {
    output.push_str("  <");
    output.push_str(tag);
    output.push('>');
    push_xml_escaped_text(output, value);
    output.push_str("</");
    output.push_str(tag);
    output.push_str(">\n");
}

fn push_xml_escaped_text(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

fn xml_value(fragment: &str, tag: &str) -> Option<String> {
    let value = between(fragment, &format!("<{tag}>"), &format!("</{tag}>"))?;
    Some(unescape_xml(value))
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start = text.find(start)? + start.len();
    let end = text[start..].find(end)? + start;
    Some(&text[start..end])
}

fn detected_execution_environment() -> ExecutionEnvironment {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => {
            ExecutionEnvironment::new(Local::now().format("%Y-%m-%d").to_string(), timezone)
        }
        Err(_) => ExecutionEnvironment::new(
            Utc::now().format("%Y-%m-%d").to_string(),
            Arc::from("Etc/UTC"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(item: &ResponseItem) -> String {
        serde_json::to_string(item).expect("context item serializes")
    }

    #[test]
    fn configured_local_time_replaces_the_embedding_host_context() {
        let configured = ExecutionEnvironment::new("2026-07-29", "/UTC");
        let snapshot = ContextSnapshot::capture("/app", "bash", None, Some(&configured));
        let rendered = text(&snapshot.full_item());

        assert!(rendered.contains("<current_date>2026-07-29</current_date>"));
        assert!(rendered.contains("<timezone>/UTC</timezone>"));
    }

    #[test]
    fn ordinary_turn_repeats_the_complete_turn_context_group() {
        let before = ContextSnapshot::capture_at(
            "/workspace",
            "zsh",
            Some("keep the prefix"),
            "2026-07-27",
            "America/Los_Angeles",
        );
        let after = ContextSnapshot::capture_at(
            "/workspace",
            "zsh",
            Some("keep the prefix"),
            "2026-07-28",
            "America/New_York",
        );
        let rendered = text(&after.diff_item(&before).expect("environment diff"));
        assert!(rendered.contains("<current_date>2026-07-28</current_date>"));
        assert!(rendered.contains("<timezone>America/New_York</timezone>"));
        assert!(
            rendered
                .contains("<filesystem><workspace_roots><root>/workspace</root></workspace_roots>")
        );
        assert!(!rendered.contains("keep the prefix"));
        assert!(!rendered.contains("<cwd>"));
        assert!(!rendered.contains("<shell>"));
    }

    #[test]
    fn agents_md_diff_is_explicit_and_reconstructable() {
        let before =
            ContextSnapshot::capture_at("/workspace", "zsh", Some("old"), "2026-07-27", "Etc/UTC");
        let after =
            ContextSnapshot::capture_at("/workspace", "zsh", Some("new"), "2026-07-27", "Etc/UTC");
        let replacement = after.diff_item(&before).expect("replacement");
        assert!(text(&replacement).contains(REPLACEMENT_NOTICE));
        let reconstructed = ContextSnapshot::reconstruct(&[before.full_item(), replacement]);
        assert_eq!(reconstructed, after);

        let removed =
            ContextSnapshot::capture_at("/workspace", "zsh", None, "2026-07-27", "Etc/UTC");
        let removed = removed.diff_item(&after).expect("removal");
        let ResponseItem::Message { content, .. } = removed else {
            panic!("removal diff is not a message");
        };
        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("removal diff does not contain exactly one text item");
        };
        assert_eq!(
            text.as_ref(),
            "# AGENTS.md instructions\n\n<INSTRUCTIONS>\n\
             The previously provided AGENTS.md instructions no longer apply.\n\
             </INSTRUCTIONS>"
        );
    }

    #[test]
    fn missing_baseline_requires_full_reinjection() {
        let snapshot = ContextSnapshot::capture_at(
            "/workspace",
            "zsh",
            Some("cached"),
            "2026-07-27",
            "Etc/UTC",
        );
        let mut state = ContextState::new(
            Some(Arc::from("cached")),
            ContextBaseline::Known(snapshot.clone()),
        );
        state.require_full_reinjection();
        let update = state.update(snapshot).expect("full context");
        assert!(update.full);
        let rendered = text(&update.item);
        assert!(rendered.contains("cached"));
        assert!(rendered.contains("<filesystem>"));
    }

    #[test]
    fn unrelated_legacy_history_does_not_fabricate_a_known_baseline() {
        let history = [ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText {
                text: "talk about <environment_context> without injecting one".into(),
            }],
        )];
        assert_eq!(
            ContextBaseline::reconstruct(&history),
            ContextBaseline::Missing
        );
    }
}
