//! One view of user and project agent context across the Codex-native
//! (`~/.codex`, `AGENTS.md`, `.agents/skills`) and Claude-native
//! (`~/.claude`, `CLAUDE.md`, `.claude/skills`, `.claude/agents`) layouts.
//!
//! Models trained on either CLI reach for their native paths. [`AgentHome`]
//! resolves both homes, exposes deterministic union views of the
//! format-identical content (instructions, skills, subagent profiles), and can
//! create natural-path symlink aliases with [`AgentHome::link_natural_paths`].
//!
//! The canonical home ([`AgentHome::codex_home`], `CODEX_HOME` or
//! `~/.codex`) remains the only place Nanocodex owns sessions and durable
//! state. Tool-owned formats ([`NATIVE_OWNED_ENTRIES`]) are never shared,
//! read, linked or modified by this crate.

mod agents;
mod fs_util;
mod instructions;
mod link;
mod project;
mod skills;

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

use serde::Serialize;

pub use agents::{
    AgentProfileFile, AgentProfileRoot, AgentProfileSet, ProfileFormat, resolve_agent_profiles,
};
pub use instructions::{
    GlobalInstructions, InstructionKind, InstructionSource, MAX_INSTRUCTION_FILE_BYTES,
};
pub use link::{LinkAction, LinkMode, LinkOutcome, LinkReport, SharedItem};
pub use project::{InstructionFile, ProjectHome, ProjectInstructions};
pub use skills::{SkillEntry, SkillRoot, SkillSet, resolve_skills};

/// Environment variable selecting the canonical Nanocodex/Codex home.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";
/// Environment variable selecting the Claude Code configuration directory.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Entries in either home whose formats belong to one native tool. They are
/// never treated as shared content and this crate never links or writes them.
pub const NATIVE_OWNED_ENTRIES: &[&str] = &[
    "settings.json",
    "settings.local.json",
    "config.toml",
    "auth.json",
    ".credentials.json",
    "sessions",
    "projects",
    "history.jsonl",
    "todos",
    "statsig",
    "shell-snapshots",
    "plugins",
];

/// Failure to locate a home directory.
#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    /// Neither the explicit override nor `HOME`/`USERPROFILE` is set.
    #[error("home directory is unavailable; set HOME (or {0})")]
    Unavailable(&'static str),
}

/// Which scope contributed an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// A user-level home (`~/.codex`, `~/.claude`, `~/.agents`).
    User,
    /// A workspace or one of its ancestors up to the project root.
    Project,
}

/// Which native CLI convention a location belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Convention {
    /// Read natively by Codex (`AGENTS.md`, `~/.codex`, `.agents/skills`).
    Codex,
    /// Read natively by Claude Code (`CLAUDE.md`, `~/.claude`, `.claude/`).
    Claude,
}

/// Severity of a resolution diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Expected deduplication or shadowing, recorded for transparency.
    Info,
    /// Something the user probably wants to fix (dangling link, unreadable file).
    Warning,
}

/// One explicit resolution note; omissions are never silent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub level: Level,
    pub path: PathBuf,
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn info(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            level: Level::Info,
            path: path.into(),
            message: message.into(),
        }
    }
    pub(crate) fn warning(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.level {
            Level::Info => "info",
            Level::Warning => "warning",
        };
        write!(f, "{level}: {}: {}", self.path.display(), self.message)
    }
}

/// The resolved canonical Nanocodex home and its Claude alias home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentHome {
    codex_home: PathBuf,
    claude_home: PathBuf,
    user_home: Option<PathBuf>,
}

impl AgentHome {
    /// Explicit homes. `~/.agents/skills` is consulted only after
    /// [`Self::with_user_home`] supplies the user's home directory.
    pub fn new(codex_home: impl Into<PathBuf>, claude_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            claude_home: claude_home.into(),
            user_home: None,
        }
    }

    /// Sets the user home used for the generic `~/.agents/skills` root.
    #[must_use]
    pub fn with_user_home(mut self, user_home: Option<PathBuf>) -> Self {
        self.user_home = user_home;
        self
    }

    /// Resolves from the process environment: `CODEX_HOME` or `~/.codex`,
    /// `CLAUDE_CONFIG_DIR` or `~/.claude`. Empty values are ignored.
    pub fn from_env() -> Result<Self, HomeError> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// [`Self::from_env`] over a caller-supplied variable lookup.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, HomeError> {
        let get = |name: &str| {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let user_home = get("HOME").or_else(|| get("USERPROFILE"));
        let codex_home = match get(CODEX_HOME_ENV) {
            Some(path) => path,
            None => user_home
                .as_ref()
                .ok_or(HomeError::Unavailable(CODEX_HOME_ENV))?
                .join(".codex"),
        };
        let claude_home = match get(CLAUDE_CONFIG_DIR_ENV) {
            Some(path) => path,
            None => user_home
                .as_ref()
                .ok_or(HomeError::Unavailable(CLAUDE_CONFIG_DIR_ENV))?
                .join(".claude"),
        };
        Ok(Self {
            codex_home,
            claude_home,
            user_home,
        })
    }

    /// Canonical Nanocodex home: the only owner of sessions and durable state.
    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    /// Claude alias home. Only shared, format-identical content is read here.
    pub fn claude_home(&self) -> &Path {
        &self.claude_home
    }

    /// User home directory, when known.
    pub fn user_home(&self) -> Option<&Path> {
        self.user_home.as_deref()
    }

    /// Project resolution rooted at `workspace` (see [`ProjectHome::discover`]).
    pub fn project(&self, workspace: impl AsRef<Path>) -> ProjectHome {
        ProjectHome::discover(workspace)
    }
}
