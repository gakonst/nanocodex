use std::{
    ffi::OsStr,
    fs::{self, DirEntry},
    path::{Path, PathBuf},
};

use nanocodex::{
    AgentSessionContext,
    agent::rollout::RolloutConfig,
    oai::responses::{ContentItem, MessageRole, ResponseItem},
};

const CURRENT_THREAD_BUDGET: usize = 1_200;
const RECENT_WORK_BUDGET: usize = 2_200;
const WORKSPACE_BUDGET: usize = 1_600;
const NOTES_BUDGET: usize = 300;
const TOTAL_BUDGET: usize = 5_300;
const TURN_BUDGET: usize = 300;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const MAX_RECENT_THREADS: usize = 40;
const MAX_RECENT_GROUPS: usize = 8;
const TREE_DEPTH: usize = 2;
const TREE_ENTRIES: usize = 20;
const NOISY_DIRS: &[&str] = &[
    ".git",
    ".next",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "out",
    "target",
];

pub(super) fn build(context: &AgentSessionContext, rollout: Option<&Path>) -> Option<String> {
    let current = current_thread(context.history());
    let recent = rollout.and_then(recent_work);
    let workspace = workspace_map(Path::new(context.workspace()));
    if current.is_none() && recent.is_none() && workspace.is_none() {
        return None;
    }

    let mut parts = vec![concat!(
        "Startup context from Codex.\n",
        "This is background context about recent work and machine/workspace layout. It may be incomplete or stale. Use it to inform responses, and do not repeat it back unless relevant."
    ).to_owned()];
    section(&mut parts, "Current Thread", current, CURRENT_THREAD_BUDGET);
    section(&mut parts, "Recent Work", recent, RECENT_WORK_BUDGET);
    section(
        &mut parts,
        "Machine / Workspace Map",
        workspace,
        WORKSPACE_BUDGET,
    );
    section(
        &mut parts,
        "Notes",
        Some("Built at realtime startup from the current thread history, local thread metadata, and a bounded local workspace scan. This excludes repo memory instructions, AGENTS files, project-doc prompt blends, and memory summaries.".to_owned()),
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

fn current_thread(history: &[ResponseItem]) -> Option<String> {
    let mut turns: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    let mut user = Vec::new();
    let mut assistant = Vec::new();
    for item in history {
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        let text = content
            .iter()
            .filter_map(|part| match part {
                ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        if text.is_empty() || contextual(&text) {
            continue;
        }
        match role {
            MessageRole::User => {
                if !user.is_empty() || !assistant.is_empty() {
                    turns.push((std::mem::take(&mut user), std::mem::take(&mut assistant)));
                }
                user.push(text);
            }
            MessageRole::Assistant if !user.is_empty() || !assistant.is_empty() => {
                assistant.push(text);
            }
            MessageRole::Developer | MessageRole::Assistant => {}
        }
    }
    if !user.is_empty() || !assistant.is_empty() {
        turns.push((user, assistant));
    }
    if turns.is_empty() {
        return None;
    }

    let mut output = String::from(
        "Most recent user/assistant turns from this exact thread. Use them for continuity when responding.",
    );
    let mut remaining = CURRENT_THREAD_BUDGET.saturating_sub(tokens(&output));
    for (index, (user, assistant)) in turns.into_iter().rev().enumerate() {
        if remaining == 0 {
            break;
        }
        let mut turn = if index == 0 {
            "### Latest turn".to_owned()
        } else {
            format!("### Previous turn {index}")
        };
        if !user.is_empty() {
            turn.push_str("\nUser:\n");
            turn.push_str(&user.join("\n\n"));
        }
        if !assistant.is_empty() {
            turn.push_str("\n\nAssistant:\n");
            turn.push_str(&assistant.join("\n\n"));
        }
        let turn = truncate(&turn, TURN_BUDGET.min(remaining));
        remaining = remaining.saturating_sub(tokens(&turn));
        output.push_str("\n\n");
        output.push_str(&turn);
    }
    Some(output)
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

fn recent_work(rollout: &Path) -> Option<String> {
    let sessions = rollout
        .ancestors()
        .find(|path| path.file_name() == Some(OsStr::new("sessions")))?;
    let codex_home = sessions.parent()?;
    let sessions = RolloutConfig::new(codex_home).list_sessions().ok()?;
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for session in sessions
        .into_iter()
        .filter(|session| !session.is_archived())
        .take(MAX_RECENT_THREADS)
    {
        let (Some(cwd), Some(ask)) = (session.workspace(), session.preview()) else {
            continue;
        };
        if let Some((_, asks)) = groups.iter_mut().find(|(group, _)| *group == cwd) {
            asks.push(ask.to_owned());
        } else if groups.len() < MAX_RECENT_GROUPS {
            groups.push((cwd.to_owned(), vec![ask.to_owned()]));
        }
    }
    (!groups.is_empty()).then(|| {
        groups
            .into_iter()
            .map(|(cwd, asks)| {
                format!(
                    "### Directory: {cwd}\nRecent sessions: {}\nRecent asks:\n{}",
                    asks.len(),
                    asks.into_iter()
                        .map(|ask| format!("- {ask}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

fn workspace_map(workspace: &Path) -> Option<String> {
    if !workspace.is_dir() {
        return None;
    }
    let git_root = workspace
        .ancestors()
        .find(|path| path.join(".git").exists())
        .map(Path::to_path_buf);
    let user_root = std::env::var_os("HOME").map(PathBuf::from);
    let mut lines = vec![
        format!("Current working directory: {}", workspace.display()),
        format!("Working directory name: {}", name(workspace)),
    ];
    if let Some(root) = &git_root {
        lines.push(format!("Git root: {}", root.display()));
        lines.push(format!("Git project: {}", name(root)));
    }
    if let Some(root) = &user_root {
        lines.push(format!("User root: {}", root.display()));
    }
    append_tree(&mut lines, "Working directory tree:", workspace);
    if let Some(root) = git_root.as_deref().filter(|root| *root != workspace) {
        append_tree(&mut lines, "Git root tree:", root);
    }
    if let Some(root) = user_root
        .as_deref()
        .filter(|root| *root != workspace && Some(*root) != git_root.as_deref())
    {
        append_tree(&mut lines, "User root tree:", root);
    }
    Some(lines.join("\n"))
}

fn append_tree(lines: &mut Vec<String>, heading: &str, root: &Path) {
    let mut tree = Vec::new();
    collect_tree(root, 0, &mut tree);
    if !tree.is_empty() {
        lines.push(String::new());
        lines.push(heading.to_owned());
        lines.extend(tree);
    }
}

fn collect_tree(root: &Path, depth: usize, output: &mut Vec<String>) {
    if depth >= TREE_DEPTH {
        return;
    }
    let Ok(mut entries) = fs::read_dir(root).map(|entries| {
        entries
            .flatten()
            .filter(|entry| !noisy(&entry.file_name()))
            .collect::<Vec<DirEntry>>()
    }) else {
        return;
    };
    entries.sort_by_key(|entry| (!entry.path().is_dir(), entry.file_name()));
    let total = entries.len();
    for entry in entries.into_iter().take(TREE_ENTRIES) {
        let path = entry.path();
        let directory = path.is_dir();
        output.push(format!(
            "{}- {}{}",
            "  ".repeat(depth),
            name(&path),
            if directory { "/" } else { "" }
        ));
        if directory {
            collect_tree(&path, depth + 1, output);
        }
    }
    if total > TREE_ENTRIES {
        output.push(format!(
            "{}- ... {} more entries",
            "  ".repeat(depth),
            total - TREE_ENTRIES
        ));
    }
}

fn noisy(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || NOISY_DIRS.iter().any(|noisy| *noisy == name)
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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
    text.len().saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

fn truncate(text: &str, budget: usize) -> String {
    let max = budget.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= max {
        return text.to_owned();
    }
    let marker = "\n…truncated…\n";
    let keep = max.saturating_sub(marker.len());
    let mut end = keep / 2;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut start = text.len().saturating_sub(keep.saturating_sub(end));
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{}{marker}{}", &text[..end], &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_thread_skips_context_and_keeps_recent_turns() {
        let history = vec![
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text(
                    "<environment_context>hidden</environment_context>",
                )],
            ),
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text("fix the parser")],
            ),
            ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text("parser fixed")],
            ),
        ];
        let context = current_thread(&history).unwrap();
        assert!(context.contains("### Latest turn"));
        assert!(context.contains("fix the parser"));
        assert!(context.contains("parser fixed"));
        assert!(!context.contains("environment_context"));
        assert!(tokens(&context) <= CURRENT_THREAD_BUDGET);
    }

    #[test]
    fn workspace_tree_is_bounded_and_filters_noisy_directories() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::create_dir(directory.path().join("target")).unwrap();
        fs::write(directory.path().join("src/lib.rs"), "fn main() {}").unwrap();
        let context = workspace_map(directory.path()).unwrap();
        assert!(context.contains("src/"));
        assert!(context.contains("lib.rs"));
        assert!(!context.contains("target/"));
    }

    #[test]
    fn recent_work_discovers_the_canonical_sessions_root() {
        let directory = tempfile::tempdir().unwrap();
        let thread_id = "019eb97d-8e9a-7ff3-94b0-ea019babd5d7";
        let sessions = directory.path().join("sessions/2026/08/23");
        fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join(format!("rollout-2026-08-23T00-00-00-{thread_id}.jsonl"));
        fs::write(
            &rollout,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread_id}\",\"cwd\":\"/work/nanocodex\",\"history_mode\":\"legacy\"}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"make voice reconnect\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{}}}}\n"
            ),
        )
        .unwrap();

        let context = recent_work(&rollout).unwrap();
        assert!(context.contains("### Directory: /work/nanocodex"));
        assert!(context.contains("- make voice reconnect"));
    }
}
