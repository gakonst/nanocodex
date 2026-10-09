//! Bounded project subagent definitions. These are requests to the host, never
//! independent authority. Hosts must enforce tool limits and inherited policy.
use crate::context::{
    FILE_BYTES, authorized_root, frontmatter, local_directory, read_local, read_user_file,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub path: String,
    pub instructions: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<String>,
    pub isolation: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentProfileCatalog {
    pub profiles: Vec<AgentProfile>,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct ClaudeAgentProfiles {
    root: PathBuf,
    user: Vec<nanocodex_home::AgentProfileRoot>,
}
impl ClaudeAgentProfiles {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self {
            root: authorized_root(root.as_ref())?,
            user: Vec::new(),
        })
    }
    /// Adds host-resolved user profile roots (for example
    /// `nanocodex_home::AgentHome::agent_profile_roots`). Only Claude Markdown
    /// roots such as `~/.claude/agents` apply; Codex TOML roots are ignored.
    /// Project profiles win over same-named user profiles, as in Claude Code.
    #[must_use]
    pub fn with_user_roots(mut self, roots: Vec<nanocodex_home::AgentProfileRoot>) -> Self {
        self.user = roots
            .into_iter()
            .filter(|root| root.format == nanocodex_home::ProfileFormat::ClaudeMarkdown)
            .collect();
        self
    }
    pub fn catalog(&self) -> AgentProfileCatalog {
        let mut catalog = AgentProfileCatalog::default();
        let mut pending = vec![PathBuf::from(".claude/agents")];
        let mut names = BTreeMap::new();
        let mut scanned = 0;
        while let Some(directory) = pending.pop() {
            if !local_directory(&self.root, &directory) {
                continue;
            }
            let entries = match fs::read_dir(self.root.join(&directory)) {
                Ok(entries) => entries,
                Err(error) => {
                    catalog
                        .diagnostics
                        .push(format!("{}: {error}", directory.display()));
                    continue;
                }
            };
            let mut files = Vec::new();
            for entry in entries {
                scanned += 1;
                if scanned > 256 {
                    catalog
                        .diagnostics
                        .push("agent discovery exceeded 256 entries".into());
                    return catalog;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        catalog.diagnostics.push(error.to_string());
                        continue;
                    }
                };
                let path = directory.join(entry.file_name());
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => pending.push(path),
                    Ok(kind) if kind.is_file() && path.extension().is_some_and(|e| e == "md") => {
                        files.push(path)
                    }
                    Ok(kind) if kind.is_symlink() => catalog.diagnostics.push(format!(
                        "{}: symlink profile paths are unsupported",
                        path.display()
                    )),
                    _ => {}
                }
            }
            files.sort();
            for path in files {
                match self.read(&path) {
                    Ok(profile) => {
                        if names.insert(profile.name.clone(), ()).is_some() {
                            catalog.profiles.retain(|p| p.name != profile.name);
                            catalog.diagnostics.push(format!(
                                "duplicate agent name {} is unavailable",
                                profile.name
                            ));
                        } else if catalog.profiles.len() < 64 {
                            catalog.profiles.push(profile);
                        } else {
                            catalog
                                .diagnostics
                                .push("agent catalog exceeded 64 profiles".into());
                        }
                    }
                    Err(error) => catalog
                        .diagnostics
                        .push(format!("{}: {error}", path.display())),
                }
            }
        }
        if !self.user.is_empty() {
            let resolved = nanocodex_home::resolve_agent_profiles(&self.user);
            for diagnostic in resolved.diagnostics {
                catalog.diagnostics.push(diagnostic.to_string());
            }
            for file in resolved.profiles {
                match self.read(&file.path) {
                    // Project definitions (and earlier user roots) take precedence.
                    Ok(profile) if names.contains_key(&profile.name) => {}
                    Ok(profile) if catalog.profiles.len() < 64 => {
                        names.insert(profile.name.clone(), ());
                        catalog.profiles.push(profile);
                    }
                    Ok(_) => catalog
                        .diagnostics
                        .push("agent catalog exceeded 64 profiles".into()),
                    Err(error) => catalog
                        .diagnostics
                        .push(format!("{}: {error}", file.path.display())),
                }
            }
        }
        catalog.profiles.sort_by(|a, b| a.name.cmp(&b.name));
        catalog
    }
    pub fn get(&self, name: &str) -> Result<AgentProfile, String> {
        let catalog = self.catalog();
        catalog
            .profiles
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                format!(
                    "agent profile {name:?} unavailable; diagnostics: {}",
                    catalog.diagnostics.join("; ")
                )
            })
    }
    /// Project profiles are workspace-relative and never follow symlinks; user
    /// profiles are absolute paths under a configured user root.
    fn read(&self, path: &Path) -> Result<AgentProfile, String> {
        let (text, truncated) = if path.is_absolute() {
            if !self.user.iter().any(|root| path.starts_with(&root.path)) {
                return Err("agent profile path is outside the configured roots".into());
            }
            read_user_file(path, FILE_BYTES)?
        } else {
            read_local(&self.root, path, FILE_BYTES)?
        };
        if truncated {
            return Err("agent profile exceeds 32 KiB".into());
        }
        let (meta, body) = frontmatter(&text)?;
        let map = meta.as_object().ok_or("agent requires YAML frontmatter")?;
        for key in map.keys() {
            if !matches!(
                key.as_str(),
                "name"
                    | "description"
                    | "tools"
                    | "disallowedTools"
                    | "model"
                    | "permissionMode"
                    | "isolation"
            ) {
                return Err(format!("unsupported agent frontmatter: {key}"));
            }
        }
        let name = string(&meta, "name")?.ok_or("agent name is required")?;
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || matches!(name.as_str(), "general-purpose" | "fork")
        {
            return Err(
                "invalid or reserved agent name (1..64 ASCII letters/digits/hyphens/underscores)"
                    .into(),
            );
        }
        let description = string(&meta, "description")?.ok_or("agent description is required")?;
        if description.trim().is_empty() || description.len() > 2048 {
            return Err("agent description must be nonblank and at most 2 KiB".into());
        }
        let model = string(&meta, "model")?.filter(|v| v != "inherit");
        if model.as_ref().is_some_and(|m| {
            !matches!(m.as_str(), "opus" | "sonnet" | "haiku" | "fable")
                && !m.starts_with("claude-")
        }) {
            return Err("agent model must be a Claude model ID, alias or inherit".into());
        }
        let permission_mode = string(&meta, "permissionMode")?;
        if permission_mode
            .as_ref()
            .is_some_and(|m| !matches!(m.as_str(), "default" | "manual" | "dontAsk" | "plan"))
        {
            return Err("agent permissionMode supports only restrictive default/manual/dontAsk/plan; project definitions cannot elevate authority".into());
        }
        let isolation = string(&meta, "isolation")?;
        if isolation.as_deref().is_some_and(|v| v != "worktree") {
            return Err("only isolation: worktree is supported".into());
        }
        Ok(AgentProfile {
            name,
            description,
            path: path.to_string_lossy().into(),
            instructions: body.into(),
            model,
            tools: tool_list(&meta, "tools")?,
            disallowed_tools: tool_list(&meta, "disallowedTools")?.unwrap_or_default(),
            permission_mode,
            isolation,
        })
    }
}
fn string(meta: &Value, key: &str) -> Result<Option<String>, String> {
    meta.get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must be a string"))
        })
        .transpose()
}
fn tool_list(meta: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    let Some(value) = meta.get(key) else {
        return Ok(None);
    };
    let tools = match value {
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(items) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(format!("{key} must be a comma-separated string or list")),
    };
    if tools.len() > 64
        || tools.iter().any(|s| {
            s.is_empty()
                || s.len() > 128
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
    {
        return Err(format!(
            "{key} supports at most 64 exact tool names; patterns/specifiers are unsupported"
        ));
    }
    Ok(Some(tools))
}
