use std::{collections::BTreeMap, path::PathBuf};

use serde::Serialize;

use crate::{
    AgentHome, Convention, Diagnostic, Scope,
    fs_util::{PathState, path_state, sorted_visible_entries},
    project::ProjectHome,
};

const MAX_PROFILE_ENTRIES: usize = 256;

/// On-disk format of a subagent profile. The formats are not interchangeable,
/// so profiles are unioned for discovery but never aliased across homes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFormat {
    /// Claude Code `agents/**/*.md` with YAML frontmatter.
    ClaudeMarkdown,
    /// Codex standalone `agents/*.toml` custom-agent config layers.
    CodexToml,
}

/// A directory of subagent profiles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentProfileRoot {
    pub path: PathBuf,
    pub scope: Scope,
    pub convention: Convention,
    pub format: ProfileFormat,
}

/// One discovered profile file. `name` is the file stem; the authoritative
/// name may live in frontmatter/TOML and is the consumer's to parse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentProfileFile {
    pub name: String,
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub root: AgentProfileRoot,
}

/// Union of profiles: one winner per (format, name), plus shadowed entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AgentProfileSet {
    pub profiles: Vec<AgentProfileFile>,
    pub shadowed: Vec<AgentProfileFile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Resolves profile roots in precedence order (first wins per format+name).
pub fn resolve_agent_profiles(roots: &[AgentProfileRoot]) -> AgentProfileSet {
    let mut set = AgentProfileSet::default();
    let mut winners: BTreeMap<(String, ProfileFormat), AgentProfileFile> = BTreeMap::new();
    let mut seen: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for root in roots {
        let (extension, max_depth) = match root.format {
            ProfileFormat::ClaudeMarkdown => ("md", 4),
            ProfileFormat::CodexToml => ("toml", 0),
        };
        let mut files = Vec::new();
        let mut pending = vec![(root.path.clone(), 0usize)];
        while let Some((dir, depth)) = pending.pop() {
            match path_state(&dir) {
                Ok(PathState::Dir { .. }) => {}
                Ok(PathState::Absent) => continue,
                Ok(PathState::Dangling) => {
                    set.diagnostics
                        .push(Diagnostic::warning(&dir, "dangling symlink ignored"));
                    continue;
                }
                Ok(_) => continue,
                Err(error) => {
                    set.diagnostics
                        .push(Diagnostic::warning(&dir, format!("unreadable: {error}")));
                    continue;
                }
            }
            let entries = match sorted_visible_entries(&dir, MAX_PROFILE_ENTRIES) {
                Ok((entries, truncated)) => {
                    if truncated {
                        set.diagnostics
                            .push(Diagnostic::warning(&dir, "profile discovery truncated"));
                    }
                    entries
                }
                Err(error) => {
                    set.diagnostics
                        .push(Diagnostic::warning(&dir, format!("unreadable: {error}")));
                    continue;
                }
            };
            for entry in entries {
                match path_state(&entry) {
                    Ok(PathState::Dir { .. }) if depth < max_depth => {
                        pending.push((entry, depth + 1));
                    }
                    Ok(PathState::File { canonical })
                        if entry.extension().is_some_and(|e| e == extension) =>
                    {
                        files.push((entry, canonical));
                    }
                    Ok(PathState::Dangling) => set
                        .diagnostics
                        .push(Diagnostic::warning(&entry, "dangling symlink ignored")),
                    _ => {}
                }
            }
        }
        files.sort();
        for (path, canonical_path) in files {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            if let Some(first) = seen.get(&canonical_path) {
                set.diagnostics.push(Diagnostic::info(
                    &path,
                    format!("same file as {}; listed once", first.display()),
                ));
                continue;
            }
            seen.insert(canonical_path.clone(), path.clone());
            let profile = AgentProfileFile {
                name,
                path,
                canonical_path,
                root: root.clone(),
            };
            let key = (profile.name.clone(), root.format);
            if let Some(winner) = winners.get(&key) {
                set.diagnostics.push(Diagnostic::info(
                    &profile.path,
                    format!(
                        "profile {} shadowed by {}",
                        profile.name,
                        winner.path.display()
                    ),
                ));
                set.shadowed.push(profile);
            } else {
                winners.insert(key, profile);
            }
        }
    }
    set.profiles = winners.into_values().collect();
    set.profiles
        .sort_by(|a, b| (&a.name, a.root.format, &a.path).cmp(&(&b.name, b.root.format, &b.path)));
    set
}

impl AgentHome {
    /// User-level profile roots: `$CLAUDE_CONFIG_DIR/agents` (Markdown) and
    /// `$CODEX_HOME/agents` (Codex TOML).
    pub fn agent_profile_roots(&self) -> Vec<AgentProfileRoot> {
        vec![
            AgentProfileRoot {
                path: self.claude_home().join("agents"),
                scope: Scope::User,
                convention: Convention::Claude,
                format: ProfileFormat::ClaudeMarkdown,
            },
            AgentProfileRoot {
                path: self.codex_home().join("agents"),
                scope: Scope::User,
                convention: Convention::Codex,
                format: ProfileFormat::CodexToml,
            },
        ]
    }

    /// Project roots first (project subagents win, as in Claude Code), then
    /// user roots.
    pub fn agent_profile_roots_for(&self, project: &ProjectHome) -> Vec<AgentProfileRoot> {
        let mut roots = project.agent_profile_roots();
        roots.extend(self.agent_profile_roots());
        roots
    }

    /// User-level profiles.
    pub fn agent_profiles(&self) -> AgentProfileSet {
        resolve_agent_profiles(&self.agent_profile_roots())
    }

    /// Project and user profiles resolved over [`Self::agent_profile_roots_for`].
    pub fn agent_profiles_for(&self, project: &ProjectHome) -> AgentProfileSet {
        resolve_agent_profiles(&self.agent_profile_roots_for(project))
    }
}
