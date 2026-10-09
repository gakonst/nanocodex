use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    AgentHome, Convention, Diagnostic, Scope,
    fs_util::{PathState, path_state, sorted_visible_entries},
    project::ProjectHome,
};

const MAX_SKILLS_PER_ROOT: usize = 512;

/// A directory whose children are `<name>/SKILL.md` skill folders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub scope: Scope,
    pub convention: Convention,
}

impl SkillRoot {
    pub(crate) const fn new(path: PathBuf, scope: Scope, convention: Convention) -> Self {
        Self {
            path,
            scope,
            convention,
        }
    }
}

/// One discovered skill folder. The name is the folder name (Claude Code's
/// rule); consumers parse `SKILL.md` frontmatter themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillEntry {
    pub name: String,
    /// The folder as found under its root (may be a symlink).
    pub dir: PathBuf,
    pub skill_md: PathBuf,
    /// Symlink-resolved folder, used for deduplication.
    pub canonical_dir: PathBuf,
    pub root: SkillRoot,
}

/// Resolved skills: one winner per name, plus every shadowed candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SkillSet {
    /// Winners, sorted by name.
    pub skills: Vec<SkillEntry>,
    /// Same-named skills from lower-precedence roots, in root order.
    pub shadowed: Vec<SkillEntry>,
    pub diagnostics: Vec<Diagnostic>,
}

impl SkillSet {
    pub fn get(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.iter().find(|skill| skill.name == name)
    }
}

/// Resolves `roots` in precedence order (first wins). Symlinked skill folders
/// are followed; a folder reached twice (same canonical path) is listed once.
pub fn resolve_skills(roots: &[SkillRoot]) -> SkillSet {
    let mut set = SkillSet::default();
    let mut winners: BTreeMap<String, SkillEntry> = BTreeMap::new();
    let mut seen_dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for root in roots {
        for entry in scan_root(root, &mut set.diagnostics) {
            if let Some(first) = seen_dirs.get(&entry.canonical_dir) {
                set.diagnostics.push(Diagnostic::info(
                    &entry.dir,
                    format!("same skill folder as {}; listed once", first.display()),
                ));
                continue;
            }
            seen_dirs.insert(entry.canonical_dir.clone(), entry.dir.clone());
            if let Some(winner) = winners.get(&entry.name) {
                set.diagnostics.push(Diagnostic::info(
                    &entry.dir,
                    format!("skill {} shadowed by {}", entry.name, winner.dir.display()),
                ));
                set.shadowed.push(entry);
            } else {
                winners.insert(entry.name.clone(), entry);
            }
        }
    }
    set.skills = winners.into_values().collect();
    set
}

pub(crate) fn scan_root(root: &SkillRoot, diagnostics: &mut Vec<Diagnostic>) -> Vec<SkillEntry> {
    match path_state(&root.path) {
        Ok(PathState::Dir { .. }) => {}
        Ok(PathState::Absent) => return Vec::new(),
        Ok(PathState::Dangling) => {
            diagnostics.push(Diagnostic::warning(
                &root.path,
                "dangling skills root ignored",
            ));
            return Vec::new();
        }
        Ok(_) => {
            diagnostics.push(Diagnostic::warning(
                &root.path,
                "skills root is not a directory",
            ));
            return Vec::new();
        }
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                &root.path,
                format!("unreadable: {error}"),
            ));
            return Vec::new();
        }
    }
    let entries = match sorted_visible_entries(&root.path, MAX_SKILLS_PER_ROOT) {
        Ok((entries, truncated)) => {
            if truncated {
                diagnostics.push(Diagnostic::warning(
                    &root.path,
                    format!("more than {MAX_SKILLS_PER_ROOT} entries; remainder ignored"),
                ));
            }
            entries
        }
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                &root.path,
                format!("unreadable: {error}"),
            ));
            return Vec::new();
        }
    };
    let mut skills = Vec::new();
    for dir in entries {
        let Some(name) = dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            diagnostics.push(Diagnostic::warning(&dir, "non-UTF-8 skill name ignored"));
            continue;
        };
        let canonical_dir = match path_state(&dir) {
            Ok(PathState::Dir { canonical }) => canonical,
            Ok(PathState::Dangling) => {
                diagnostics.push(Diagnostic::warning(&dir, "dangling skill symlink ignored"));
                continue;
            }
            Ok(_) => continue,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(&dir, format!("unreadable: {error}")));
                continue;
            }
        };
        let skill_md = dir.join("SKILL.md");
        match path_state(&skill_md) {
            Ok(PathState::File { .. }) => skills.push(SkillEntry {
                name,
                dir,
                skill_md,
                canonical_dir,
                root: root.clone(),
            }),
            Ok(PathState::Dangling) => {
                diagnostics.push(Diagnostic::warning(&skill_md, "dangling SKILL.md ignored"));
            }
            _ => diagnostics.push(Diagnostic::info(&dir, "folder has no SKILL.md; ignored")),
        }
    }
    skills
}

pub(crate) fn user_skill_roots(home: &AgentHome) -> Vec<SkillRoot> {
    let mut roots = vec![
        SkillRoot::new(
            home.codex_home().join("skills"),
            Scope::User,
            Convention::Codex,
        ),
        SkillRoot::new(
            home.claude_home().join("skills"),
            Scope::User,
            Convention::Claude,
        ),
    ];
    if let Some(user) = home.user_home() {
        roots.push(SkillRoot::new(
            user.join(".agents").join("skills"),
            Scope::User,
            Convention::Codex,
        ));
    }
    roots
}

impl AgentHome {
    /// User-level skill roots in precedence order: `$CODEX_HOME/skills`,
    /// `$CLAUDE_CONFIG_DIR/skills`, then `~/.agents/skills` (when the user
    /// home is known). Roots need not exist.
    pub fn skill_roots(&self) -> Vec<SkillRoot> {
        user_skill_roots(self)
    }

    /// User-level skills resolved over [`Self::skill_roots`].
    pub fn skills(&self) -> SkillSet {
        resolve_skills(&self.skill_roots())
    }

    /// User and project skill roots: user roots first (personal skills win,
    /// as in Claude Code), then [`ProjectHome::skill_roots`].
    pub fn skill_roots_for(&self, project: &ProjectHome) -> Vec<SkillRoot> {
        let mut roots = self.skill_roots();
        roots.extend(project.skill_roots());
        roots
    }

    /// Skills resolved over [`Self::skill_roots_for`].
    pub fn skills_for(&self, project: &ProjectHome) -> SkillSet {
        resolve_skills(&self.skill_roots_for(project))
    }
}

pub(crate) fn is_skill_dir(path: &Path) -> Option<PathBuf> {
    match (path_state(path), path_state(&path.join("SKILL.md"))) {
        (Ok(PathState::Dir { canonical }), Ok(PathState::File { .. })) => Some(canonical),
        _ => None,
    }
}
