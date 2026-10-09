use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    AgentHome,
    fs_util::{PathState, path_state, sorted_visible_entries},
    skills::{is_skill_dir, user_skill_roots},
};

const MAX_LINK_SCAN: usize = 512;

/// Whether [`AgentHome::link_natural_paths`] may touch the filesystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkMode {
    /// Plan only: report `WouldCreate` and directories that would be created.
    DryRun,
    /// Create missing symlinks (and only the parent directories they need).
    Apply,
}

/// The shared content an action concerns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SharedItem {
    /// `$CODEX_HOME/AGENTS.md` <-> `$CLAUDE_CONFIG_DIR/CLAUDE.md`.
    GlobalInstructions,
    /// `$CODEX_HOME/skills/<name>` <-> `$CLAUDE_CONFIG_DIR/skills/<name>`.
    Skill { name: String },
}

/// Result for one natural path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LinkOutcome {
    /// The symlink was created.
    Created,
    /// Dry run: the symlink would be created.
    WouldCreate,
    /// The natural path already resolves to the shared source.
    AlreadyLinked,
    /// Both sides hold different content; left alone (union resolution covers it).
    BothExist,
    /// A dangling symlink occupies the path; never replaced or removed.
    DanglingLink,
    /// The path or its parent is occupied by something unexpected.
    Blocked { reason: String },
    /// Symlinks are not created on this platform.
    Unsupported,
    /// Creating the symlink failed.
    Failed { error: String },
}

/// One natural path considered by the linker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkAction {
    pub item: SharedItem,
    /// The natural path (the would-be or existing symlink location).
    pub link: PathBuf,
    /// The canonical shared source, when one exists.
    pub target: Option<PathBuf>,
    #[serde(flatten)]
    pub outcome: LinkOutcome,
}

/// Everything [`AgentHome::link_natural_paths`] did or would do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkReport {
    pub mode: LinkMode,
    pub actions: Vec<LinkAction>,
    /// Parent directories created (or, in a dry run, that would be created).
    pub directories: Vec<PathBuf>,
}

impl LinkReport {
    /// Actions that created (or would create) a symlink.
    pub fn changes(&self) -> impl Iterator<Item = &LinkAction> {
        self.actions
            .iter()
            .filter(|a| matches!(a.outcome, LinkOutcome::Created | LinkOutcome::WouldCreate))
    }

    /// Actions needing user attention: dangling, blocked or failed paths.
    pub fn problems(&self) -> impl Iterator<Item = &LinkAction> {
        self.actions.iter().filter(|a| {
            matches!(
                a.outcome,
                LinkOutcome::DanglingLink
                    | LinkOutcome::Blocked { .. }
                    | LinkOutcome::Failed { .. }
            )
        })
    }
}

struct Planner {
    mode: LinkMode,
    report: LinkReport,
}

impl Planner {
    fn note(
        &mut self,
        item: SharedItem,
        link: PathBuf,
        target: Option<PathBuf>,
        outcome: LinkOutcome,
    ) {
        self.report.actions.push(LinkAction {
            item,
            link,
            target,
            outcome,
        });
    }

    /// Ensures `dir` exists as a directory, creating only what is missing.
    fn ensure_dir(&mut self, dir: &Path) -> Result<(), String> {
        if self.report.directories.iter().any(|d| d == dir) {
            return Ok(());
        }
        match path_state(dir) {
            Ok(PathState::Dir { .. }) => Ok(()),
            Ok(PathState::Absent) => {
                if self.mode == LinkMode::Apply {
                    fs::create_dir_all(dir)
                        .map_err(|e| format!("create {}: {e}", dir.display()))?;
                }
                self.report.directories.push(dir.to_path_buf());
                Ok(())
            }
            Ok(PathState::Dangling) => Err(format!("{} is a dangling symlink", dir.display())),
            Ok(_) => Err(format!("{} is not a directory", dir.display())),
            Err(error) => Err(format!("{}: {error}", dir.display())),
        }
    }

    fn create(&mut self, item: SharedItem, link: PathBuf, target: PathBuf) {
        let outcome = self.create_outcome(&link, &target);
        self.note(item, link, Some(target), outcome);
    }

    fn create_outcome(&mut self, link: &Path, target: &Path) -> LinkOutcome {
        if !cfg!(unix) {
            return LinkOutcome::Unsupported;
        }
        if let Some(parent) = link.parent()
            && let Err(reason) = self.ensure_dir(parent)
        {
            return LinkOutcome::Blocked { reason };
        }
        if self.mode == LinkMode::DryRun {
            return LinkOutcome::WouldCreate;
        }
        match symlink(target, link) {
            Ok(()) => LinkOutcome::Created,
            // symlink(2) never replaces an existing entry; a racing writer wins.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => LinkOutcome::Blocked {
                reason: "path appeared concurrently; left untouched".into(),
            },
            Err(error) => LinkOutcome::Failed {
                error: error.to_string(),
            },
        }
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlinks are unsupported",
    ))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn state(path: &Path) -> PathState {
    // Unreadable paths are treated as occupied by something unexpected.
    path_state(path).unwrap_or(PathState::Other)
}

impl AgentHome {
    /// Makes shared content visible at each CLI's natural path by creating
    /// symlinks on the missing side, pointing at the existing side's
    /// canonical path:
    ///
    /// - `$CLAUDE_CONFIG_DIR/CLAUDE.md -> $CODEX_HOME/AGENTS.md` (or
    ///   `AGENTS.override.md` when it is the only Codex file), or
    ///   `$CODEX_HOME/AGENTS.md -> $CLAUDE_CONFIG_DIR/CLAUDE.md` when Codex has
    ///   no global instructions file.
    /// - Per skill: `$CODEX_HOME/skills/<n>` and `$CLAUDE_CONFIG_DIR/skills/<n>`
    ///   each link to the highest-precedence real folder among
    ///   `$CODEX_HOME/skills`, `$CLAUDE_CONFIG_DIR/skills`, `~/.agents/skills`.
    ///
    /// Never overwrites, removes or follows into existing entries; never
    /// touches [`crate::NATIVE_OWNED_ENTRIES`]; leaves both-sides-exist and
    /// dangling paths alone and reports them. Idempotent. Subagent profiles
    /// are not linked because their formats differ. On non-Unix platforms
    /// every planned link reports [`LinkOutcome::Unsupported`].
    pub fn link_natural_paths(&self, mode: LinkMode) -> LinkReport {
        let mut planner = Planner {
            mode,
            report: LinkReport {
                mode,
                actions: Vec::new(),
                directories: Vec::new(),
            },
        };
        self.link_instructions(&mut planner);
        self.link_skills(&mut planner);
        planner.report
    }

    fn link_instructions(&self, planner: &mut Planner) {
        let item = || SharedItem::GlobalInstructions;
        let claude = self.claude_home().join("CLAUDE.md");
        let agents = self.codex_home().join("AGENTS.md");
        let agents_override = self.codex_home().join("AGENTS.override.md");
        let (claude_state, agents_state, override_state) =
            (state(&claude), state(&agents), state(&agents_override));
        for (path, state) in [(&claude, &claude_state), (&agents, &agents_state)] {
            if *state == PathState::Dangling {
                planner.note(item(), path.clone(), None, LinkOutcome::DanglingLink);
            }
        }
        let file = |state: &PathState| match state {
            PathState::File { canonical } => Some(canonical.clone()),
            _ => None,
        };
        let codex_source = file(&agents_state).or_else(|| file(&override_state));
        let claude_source = file(&claude_state);
        match (claude_source, codex_source) {
            (Some(claude_file), Some(codex_file)) => {
                let outcome = if claude_file == codex_file {
                    LinkOutcome::AlreadyLinked
                } else {
                    LinkOutcome::BothExist
                };
                let link = if is_symlink(&claude) || outcome == LinkOutcome::BothExist {
                    claude
                } else {
                    agents
                };
                planner.note(item(), link, Some(codex_file), outcome);
            }
            (None, Some(codex_file)) => match claude_state {
                PathState::Absent => planner.create(item(), claude, codex_file),
                PathState::Dangling => {}
                _ => planner.note(
                    item(),
                    claude,
                    Some(codex_file),
                    LinkOutcome::Blocked {
                        reason: "not a regular file".into(),
                    },
                ),
            },
            (Some(claude_file), None) => {
                if agents_state == PathState::Absent && override_state == PathState::Absent {
                    planner.create(item(), agents, claude_file);
                } else if agents_state != PathState::Dangling {
                    planner.note(
                        item(),
                        agents,
                        Some(claude_file),
                        LinkOutcome::Blocked {
                            reason: "not a regular file".into(),
                        },
                    );
                }
            }
            (None, None) => {}
        }
    }

    fn link_skills(&self, planner: &mut Planner) {
        let roots = user_skill_roots(self);
        // Writable natural locations; `~/.agents/skills` is a source only.
        let sides = [
            self.codex_home().join("skills"),
            self.claude_home().join("skills"),
        ];
        let mut names = BTreeSet::new();
        for root in &roots {
            if let Ok((entries, _)) = sorted_visible_entries(&root.path, MAX_LINK_SCAN) {
                names.extend(
                    entries
                        .into_iter()
                        .filter_map(|e| e.file_name().map(ToOwned::to_owned)),
                );
            }
        }
        for name in names {
            let Some(label) = name.to_str().map(str::to_owned) else {
                continue;
            };
            let item = || SharedItem::Skill {
                name: label.clone(),
            };
            let source = roots
                .iter()
                .map(|root| root.path.join(&name))
                .find_map(|path| is_skill_dir(&path).map(|canonical| (path, canonical)));
            for side in &sides {
                let link = side.join(&name);
                match (state(&link), &source) {
                    (PathState::Dangling, _) => planner.note(
                        item(),
                        link,
                        source.as_ref().map(|s| s.1.clone()),
                        LinkOutcome::DanglingLink,
                    ),
                    (PathState::Absent, Some((_, canonical))) => {
                        planner.create(item(), link, canonical.clone());
                    }
                    (_, None) => {}
                    (occupied, Some((_, canonical))) => {
                        if occupied.canonical() == Some(canonical.as_path()) {
                            // The real source is silent; an alias to it is reported.
                            if is_symlink(&link) {
                                planner.note(
                                    item(),
                                    link,
                                    Some(canonical.clone()),
                                    LinkOutcome::AlreadyLinked,
                                );
                            }
                            continue;
                        }
                        let outcome = if matches!(occupied, PathState::Dir { .. }) {
                            LinkOutcome::BothExist
                        } else {
                            LinkOutcome::Blocked {
                                reason: "not a skill directory".into(),
                            }
                        };
                        planner.note(item(), link, Some(canonical.clone()), outcome);
                    }
                }
            }
        }
    }
}
