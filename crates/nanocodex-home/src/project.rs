use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    Convention, Diagnostic, Scope,
    agents::{AgentProfileRoot, ProfileFormat},
    fs_util::{PathState, path_state},
    instructions::{
        InstructionKind, InstructionSource, Loaded, collect_rules, combine, load_candidate,
        push_unique,
    },
    skills::SkillRoot,
};

/// A workspace and its project root: the nearest ancestor containing `.git`
/// (as Codex resolves it), or the workspace itself outside a repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectHome {
    root: PathBuf,
    workspace: PathBuf,
}

/// One project instruction file location, not yet read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    /// Directory whose subtree the file applies to.
    pub directory: PathBuf,
    pub kind: InstructionKind,
    pub convention: Convention,
}

/// Bounded project instruction texts in root-to-leaf order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProjectInstructions {
    pub sources: Vec<InstructionSource>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ProjectInstructions {
    /// Source texts joined by a blank line, or `None` when there are none.
    pub fn combined(&self) -> Option<String> {
        combine(&self.sources)
    }
}

impl ProjectHome {
    /// Locates the project root for `workspace` (canonicalized when possible).
    pub fn discover(workspace: impl AsRef<Path>) -> Self {
        let workspace = workspace.as_ref();
        let workspace = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let root = workspace
            .ancestors()
            .find(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())
            .unwrap_or(&workspace)
            .to_path_buf();
        Self { root, workspace }
    }

    /// Explicit root and workspace; `workspace` must be inside `root`.
    pub fn new(root: impl Into<PathBuf>, workspace: impl Into<PathBuf>) -> io::Result<Self> {
        let (root, workspace) = (root.into(), workspace.into());
        if !workspace.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace is outside root",
            ));
        }
        Ok(Self { root, workspace })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Root first, then each directory down to the workspace.
    pub fn directories(&self) -> Vec<PathBuf> {
        let mut dirs = self
            .workspace
            .ancestors()
            .take_while(|dir| dir.starts_with(&self.root))
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        dirs.reverse();
        dirs
    }

    /// Instruction files, root-to-leaf. Per directory: `AGENTS.override.md`
    /// if present else `AGENTS.md` (Codex's rule), then `CLAUDE.md`,
    /// `CLAUDE.local.md`, `.claude/CLAUDE.md`, then `.claude/rules/**/*.md`
    /// sorted. The same file reached twice (e.g. `CLAUDE.md -> AGENTS.md`) is
    /// listed once.
    pub fn instruction_files(&self) -> (Vec<InstructionFile>, Vec<Diagnostic>) {
        let mut files: Vec<InstructionFile> = Vec::new();
        let mut diagnostics = Vec::new();
        for directory in self.directories() {
            let mut candidates = Vec::new();
            let override_path = directory.join("AGENTS.override.md");
            let codex = match path_state(&override_path) {
                Ok(PathState::File { .. }) => (override_path, InstructionKind::AgentsOverride),
                _ => (directory.join("AGENTS.md"), InstructionKind::Agents),
            };
            candidates.push(codex);
            candidates.push((directory.join("CLAUDE.md"), InstructionKind::Claude));
            candidates.push((
                directory.join("CLAUDE.local.md"),
                InstructionKind::ClaudeLocal,
            ));
            candidates.push((
                directory.join(".claude/CLAUDE.md"),
                InstructionKind::ClaudeDir,
            ));
            let mut rules = Vec::new();
            collect_rules(
                &directory.join(".claude/rules"),
                &mut rules,
                &mut diagnostics,
            );
            candidates.extend(rules.into_iter().map(|rule| (rule, InstructionKind::Rule)));
            for (path, kind) in candidates {
                let canonical_path = match path_state(&path) {
                    Ok(PathState::File { canonical }) => canonical,
                    Ok(PathState::Absent) => continue,
                    Ok(PathState::Dangling) => {
                        diagnostics.push(Diagnostic::warning(&path, "dangling symlink ignored"));
                        continue;
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        diagnostics
                            .push(Diagnostic::warning(&path, format!("unreadable: {error}")));
                        continue;
                    }
                };
                if let Some(first) = files.iter().find(|f| f.canonical_path == canonical_path) {
                    diagnostics.push(Diagnostic::info(
                        &path,
                        format!("same file as {}; listed once", first.path.display()),
                    ));
                    continue;
                }
                files.push(InstructionFile {
                    path,
                    canonical_path,
                    directory: directory.clone(),
                    kind,
                    convention: kind.convention(),
                });
            }
        }
        (files, diagnostics)
    }

    /// Reads non-rule instruction files root-to-leaf, each whole: the model's
    /// context window, not a fixed byte budget, bounds what fits. Rules are
    /// path-scoped; read them via [`Self::instruction_files`]. Identical
    /// texts are included once.
    pub fn read_instructions(&self) -> ProjectInstructions {
        let (files, mut diagnostics) = self.instruction_files();
        let mut sources = Vec::new();
        for file in files
            .into_iter()
            .filter(|f| f.kind != InstructionKind::Rule)
        {
            if let Loaded::Source(source) = load_candidate(
                &file.path,
                Scope::Project,
                file.kind,
                usize::MAX,
                &mut diagnostics,
            ) {
                push_unique(&mut sources, source, &mut diagnostics);
            }
        }
        ProjectInstructions {
            sources,
            diagnostics,
        }
    }

    /// Project skill roots in precedence order: nearest directory first; per
    /// directory `.claude/skills` before `.agents/skills` (Claude-native
    /// entries win over the generic location, as in nanocodex-claude-tools).
    pub fn skill_roots(&self) -> Vec<SkillRoot> {
        let mut roots = Vec::new();
        for directory in self.directories().into_iter().rev() {
            roots.push(SkillRoot::new(
                directory.join(".claude/skills"),
                Scope::Project,
                Convention::Claude,
            ));
            roots.push(SkillRoot::new(
                directory.join(".agents/skills"),
                Scope::Project,
                Convention::Codex,
            ));
        }
        roots
    }

    /// Project subagent roots, nearest directory first: `.claude/agents`
    /// (Markdown) and `.codex/agents` (Codex TOML).
    pub fn agent_profile_roots(&self) -> Vec<AgentProfileRoot> {
        let mut roots = Vec::new();
        for directory in self.directories().into_iter().rev() {
            roots.push(AgentProfileRoot {
                path: directory.join(".claude/agents"),
                scope: Scope::Project,
                convention: Convention::Claude,
                format: ProfileFormat::ClaudeMarkdown,
            });
            roots.push(AgentProfileRoot {
                path: directory.join(".codex/agents"),
                scope: Scope::Project,
                convention: Convention::Codex,
                format: ProfileFormat::CodexToml,
            });
        }
        roots
    }
}
