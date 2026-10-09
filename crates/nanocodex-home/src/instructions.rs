use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    AgentHome, Convention, Diagnostic, Scope,
    fs_util::{PathState, path_state, read_bounded, sorted_visible_entries},
};

/// Per-file read bound, matching Codex's AGENTS.md and Claude's context limits.
pub const MAX_INSTRUCTION_FILE_BYTES: usize = 32 * 1024;
const MAX_RULE_FILES: usize = 256;

/// Which conventional file an instruction source came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionKind {
    /// `AGENTS.override.md`: replaces `AGENTS.md` in the same directory.
    AgentsOverride,
    /// `AGENTS.md`.
    Agents,
    /// `CLAUDE.md`.
    Claude,
    /// `CLAUDE.local.md` (personal, normally gitignored).
    ClaudeLocal,
    /// `.claude/CLAUDE.md`.
    ClaudeDir,
    /// `.claude/rules/**/*.md` (may be path-scoped via `paths` frontmatter).
    Rule,
}

impl InstructionKind {
    pub(crate) const fn convention(self) -> Convention {
        match self {
            Self::AgentsOverride | Self::Agents => Convention::Codex,
            _ => Convention::Claude,
        }
    }
}

/// One loaded instruction document and its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstructionSource {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub scope: Scope,
    pub convention: Convention,
    pub kind: InstructionKind,
    /// Trimmed document text (frontmatter and `@imports` are not interpreted).
    pub text: String,
    pub truncated: bool,
}

/// Union of user-level instructions from both homes.
///
/// Deterministic order: the Codex home's effective file first (`AGENTS.override.md`
/// when non-empty, otherwise `AGENTS.md`, exactly as Codex resolves it), then
/// the Claude home's `CLAUDE.md`. A source that is the same file (by canonical
/// path, e.g. a natural-path symlink) or has identical text is included once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GlobalInstructions {
    pub sources: Vec<InstructionSource>,
    /// User-level `~/.claude/rules/**/*.md`, sorted. Not read: rules can be
    /// path-scoped, so consumers match frontmatter themselves.
    pub rule_files: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

impl GlobalInstructions {
    /// Source texts joined by a blank line, or `None` when there are none.
    pub fn combined(&self) -> Option<String> {
        combine(&self.sources)
    }
}

pub(crate) fn combine(sources: &[InstructionSource]) -> Option<String> {
    (!sources.is_empty()).then(|| {
        sources
            .iter()
            .map(|source| source.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

pub(crate) enum Loaded {
    Source(InstructionSource),
    /// Present but empty after trimming.
    Empty,
    Missing,
}

/// Loads one candidate file with symlinks followed (user homes are trusted).
pub(crate) fn load_candidate(
    path: &Path,
    scope: Scope,
    kind: InstructionKind,
    limit: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Loaded {
    let canonical = match path_state(path) {
        Ok(PathState::File { canonical }) => canonical,
        Ok(PathState::Absent) => return Loaded::Missing,
        Ok(PathState::Dangling) => {
            diagnostics.push(Diagnostic::warning(path, "dangling symlink ignored"));
            return Loaded::Missing;
        }
        Ok(_) => {
            diagnostics.push(Diagnostic::warning(path, "not a regular file; ignored"));
            return Loaded::Missing;
        }
        Err(error) => {
            diagnostics.push(Diagnostic::warning(path, format!("unreadable: {error}")));
            return Loaded::Missing;
        }
    };
    match read_bounded(&canonical, limit) {
        Ok((text, truncated)) => {
            if truncated {
                diagnostics.push(Diagnostic::warning(
                    path,
                    format!("exceeds {limit} bytes; truncated"),
                ));
            }
            let text = text.trim();
            if text.is_empty() {
                return Loaded::Empty;
            }
            Loaded::Source(InstructionSource {
                path: path.to_path_buf(),
                canonical_path: canonical,
                scope,
                convention: kind.convention(),
                kind,
                text: text.to_owned(),
                truncated,
            })
        }
        Err(error) => {
            diagnostics.push(Diagnostic::warning(path, format!("unreadable: {error}")));
            Loaded::Missing
        }
    }
}

/// Appends unless an equal file or identical text was already included.
pub(crate) fn push_unique(
    sources: &mut Vec<InstructionSource>,
    source: InstructionSource,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if let Some(existing) = sources
        .iter()
        .find(|s| s.canonical_path == source.canonical_path)
    {
        diagnostics.push(Diagnostic::info(
            &source.path,
            format!("same file as {}; included once", existing.path.display()),
        ));
        return false;
    }
    if let Some(existing) = sources.iter().find(|s| s.text == source.text) {
        diagnostics.push(Diagnostic::info(
            &source.path,
            format!("identical to {}; included once", existing.path.display()),
        ));
        return false;
    }
    sources.push(source);
    true
}

pub(crate) fn collect_rules(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if !matches!(path_state(&directory), Ok(PathState::Dir { .. })) {
            continue;
        }
        let entries = match sorted_visible_entries(&directory, MAX_RULE_FILES) {
            Ok((entries, truncated)) => {
                if truncated {
                    diagnostics.push(Diagnostic::warning(&directory, "rule discovery truncated"));
                }
                entries
            }
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    &directory,
                    format!("unreadable: {error}"),
                ));
                continue;
            }
        };
        for entry in entries {
            match path_state(&entry) {
                Ok(PathState::Dir { .. }) if depth < 8 => pending.push((entry, depth + 1)),
                Ok(PathState::File { .. }) if entry.extension().is_some_and(|e| e == "md") => {
                    if files.len() < MAX_RULE_FILES {
                        files.push(entry);
                    }
                }
                Ok(PathState::Dangling) => {
                    diagnostics.push(Diagnostic::warning(&entry, "dangling symlink ignored"));
                }
                _ => {}
            }
        }
    }
    files.sort();
}

impl AgentHome {
    /// Union of global instructions across both homes (see [`GlobalInstructions`]).
    pub fn global_instructions(&self) -> GlobalInstructions {
        let mut result = GlobalInstructions::default();
        let diagnostics = &mut result.diagnostics;
        for (name, kind) in [
            ("AGENTS.override.md", InstructionKind::AgentsOverride),
            ("AGENTS.md", InstructionKind::Agents),
        ] {
            let path = self.codex_home().join(name);
            match load_candidate(
                &path,
                Scope::User,
                kind,
                MAX_INSTRUCTION_FILE_BYTES,
                diagnostics,
            ) {
                Loaded::Source(source) => {
                    push_unique(&mut result.sources, source, diagnostics);
                    break;
                }
                Loaded::Empty | Loaded::Missing => {}
            }
        }
        let path = self.claude_home().join("CLAUDE.md");
        if let Loaded::Source(source) = load_candidate(
            &path,
            Scope::User,
            InstructionKind::Claude,
            MAX_INSTRUCTION_FILE_BYTES,
            diagnostics,
        ) {
            push_unique(&mut result.sources, source, diagnostics);
        }
        collect_rules(
            &self.claude_home().join("rules"),
            &mut result.rule_files,
            diagnostics,
        );
        result
    }
}
