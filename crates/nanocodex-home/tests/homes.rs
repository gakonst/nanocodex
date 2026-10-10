//! Black-box journeys over real temporary homes and workspaces: real files,
//! real symlinks, public API only. The real ~/.codex and ~/.claude are never
//! touched.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use nanocodex_home::{
    AgentHome, Convention, InstructionKind, Level, LinkMode, LinkOutcome, ProfileFormat,
    ProjectHome, Scope, SharedItem,
};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    base: PathBuf,
    home: AgentHome,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let home = AgentHome::new(base.join(".codex"), base.join(".claude"))
            .with_user_home(Some(base.clone()));
        Self {
            _dir: dir,
            base,
            home,
        }
    }
    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.base.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    fn skill(&self, root: &str, name: &str, body: &str) -> PathBuf {
        self.write(
            &format!("{root}/{name}/SKILL.md"),
            &format!("---\nname: {name}\n---\n{body}\n"),
        );
        self.base.join(root).join(name)
    }
    #[cfg(unix)]
    fn symlink(&self, target: &Path, link: &str) {
        let link = self.base.join(link);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, link).unwrap();
    }
    /// Every entry under the fixture without following symlinks.
    fn snapshot(&self) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        let mut pending = vec![self.base.clone()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let value = if meta.file_type().is_symlink() {
                    format!("link -> {}", fs::read_link(&path).unwrap().display())
                } else if meta.is_dir() {
                    pending.push(path.clone());
                    "dir".into()
                } else {
                    format!("file {}", fs::read_to_string(&path).unwrap())
                };
                out.insert(path.strip_prefix(&self.base).unwrap().to_path_buf(), value);
            }
        }
        out
    }
}

fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: BTreeMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    move |key| map.get(key).cloned()
}

#[test]
fn homes_resolve_from_environment_with_overrides_and_fallbacks() {
    let home = AgentHome::from_vars(vars(&[("HOME", "/u")])).unwrap();
    assert_eq!(home.codex_home(), Path::new("/u/.codex"));
    assert_eq!(home.claude_home(), Path::new("/u/.claude"));
    assert_eq!(home.user_home(), Some(Path::new("/u")));

    let home = AgentHome::from_vars(vars(&[
        ("HOME", "/u"),
        ("CODEX_HOME", "/c"),
        ("CLAUDE_CONFIG_DIR", "/k"),
    ]))
    .unwrap();
    assert_eq!(
        (home.codex_home(), home.claude_home()),
        (Path::new("/c"), Path::new("/k"))
    );

    // Empty overrides are ignored, USERPROFILE is a fallback, no home is an error.
    let home = AgentHome::from_vars(vars(&[("CODEX_HOME", ""), ("USERPROFILE", "/w")])).unwrap();
    assert_eq!(home.codex_home(), Path::new("/w/.codex"));
    let error = AgentHome::from_vars(vars(&[("CLAUDE_CONFIG_DIR", "/k")])).unwrap_err();
    assert!(error.to_string().contains("CODEX_HOME"), "{error}");
}

#[test]
fn global_instructions_union_codex_then_claude_with_override_and_dedup() {
    let f = Fixture::new();
    assert!(f.home.global_instructions().combined().is_none());

    f.write(".codex/AGENTS.md", "codex global\n");
    f.write(".claude/CLAUDE.md", "  claude global  ");
    f.write(".claude/rules/b.md", "rule b");
    f.write(".claude/rules/nested/a.md", "rule a");
    let global = f.home.global_instructions();
    assert_eq!(
        global.combined().as_deref(),
        Some("codex global\n\nclaude global")
    );
    assert_eq!(
        global
            .sources
            .iter()
            .map(|s| (s.kind, s.convention, s.scope))
            .collect::<Vec<_>>(),
        [
            (InstructionKind::Agents, Convention::Codex, Scope::User),
            (InstructionKind::Claude, Convention::Claude, Scope::User),
        ]
    );
    assert_eq!(
        global.rule_files,
        [
            f.base.join(".claude/rules/b.md"),
            f.base.join(".claude/rules/nested/a.md")
        ]
    );

    // A non-empty override replaces AGENTS.md (Codex's rule); an empty one does not.
    f.write(".codex/AGENTS.override.md", " \n");
    assert_eq!(f.home.global_instructions().sources[0].text, "codex global");
    f.write(".codex/AGENTS.override.md", "override");
    assert_eq!(
        f.home.global_instructions().combined().as_deref(),
        Some("override\n\nclaude global")
    );

    // Identical text in both homes is included once, with an info diagnostic.
    fs::remove_file(f.base.join(".codex/AGENTS.override.md")).unwrap();
    f.write(".claude/CLAUDE.md", "codex global");
    let global = f.home.global_instructions();
    assert_eq!(global.sources.len(), 1);
    assert!(
        global
            .diagnostics
            .iter()
            .any(|d| d.level == Level::Info && d.message.contains("identical"))
    );
}

#[cfg(unix)]
#[test]
fn global_instructions_follow_symlinks_once_and_report_dangling() {
    let f = Fixture::new();
    let agents = f.write(".codex/AGENTS.md", "shared");
    f.symlink(&agents, ".claude/CLAUDE.md");
    let global = f.home.global_instructions();
    assert_eq!(global.combined().as_deref(), Some("shared"));
    assert!(
        global
            .diagnostics
            .iter()
            .any(|d| d.message.contains("same file"))
    );

    fs::remove_file(&agents).unwrap();
    let global = f.home.global_instructions();
    assert!(global.sources.is_empty());
    assert!(global.diagnostics.iter().any(|d| d.level == Level::Warning
        && d.path == f.base.join(".claude/CLAUDE.md")
        && d.message.contains("dangling")));
}

#[cfg(unix)]
#[test]
fn skills_union_has_deterministic_precedence_and_dedup() {
    let f = Fixture::new();
    f.skill(".codex/skills", "shared", "codex wins");
    f.skill(".claude/skills", "shared", "claude shadowed");
    f.skill(".claude/skills", "claude-only", "c");
    f.skill(".agents/skills", "generic", "g");
    f.skill(".agents/skills", "shared", "generic shadowed");
    let real = f.skill(".codex/skills", "linked", "l");
    f.symlink(&real, ".claude/skills/linked");
    f.symlink(&f.base.join("missing"), ".claude/skills/dangling");
    fs::create_dir_all(f.base.join(".claude/skills/not-a-skill")).unwrap();
    fs::create_dir_all(f.base.join(".codex/skills/.system/hidden")).unwrap();

    let set = f.home.skills();
    let names: Vec<_> = set.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["claude-only", "generic", "linked", "shared"]);
    let shared = set.get("shared").unwrap();
    assert_eq!(shared.root.path, f.base.join(".codex/skills"));
    assert_eq!(
        set.shadowed.iter().filter(|s| s.name == "shared").count(),
        2
    );
    assert_eq!(set.get("linked").unwrap().dir, real);
    assert!(
        set.diagnostics
            .iter()
            .any(|d| d.message.contains("same skill folder"))
    );
    assert!(
        set.diagnostics
            .iter()
            .any(|d| d.level == Level::Warning && d.message.contains("dangling"))
    );
    assert!(
        set.diagnostics
            .iter()
            .any(|d| d.message.contains("no SKILL.md"))
    );
    assert!(set.get(".system").is_none());

    // Project: user skills win; nearest directory first; .claude before .agents.
    let repo = f.base.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    f.skill("repo/.agents/skills", "shared", "project shadowed");
    f.skill("repo/.agents/skills", "proj", "root agents");
    f.skill("repo/.claude/skills", "proj", "root claude");
    f.skill("repo/sub/.agents/skills", "proj", "nearest agents");
    let project = ProjectHome::discover(repo.join("sub"));
    assert_eq!(project.root(), repo);
    let set = f.home.skills_for(&project);
    assert_eq!(set.get("shared").unwrap().root.scope, Scope::User);
    assert_eq!(
        set.get("proj").unwrap().dir,
        repo.join("sub/.agents/skills/proj")
    );
    let order: Vec<_> = project.skill_roots().into_iter().map(|r| r.path).collect();
    assert_eq!(
        order,
        [
            repo.join("sub/.claude/skills"),
            repo.join("sub/.agents/skills"),
            repo.join(".claude/skills"),
            repo.join(".agents/skills"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn project_instructions_cover_both_conventions_root_to_leaf() {
    let f = Fixture::new();
    let repo = f.base.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let agents = f.write("repo/AGENTS.md", "root agents");
    f.symlink(&agents, "repo/CLAUDE.md");
    f.write("repo/CLAUDE.local.md", "root local");
    f.write("repo/.claude/CLAUDE.md", "root dot-claude");
    f.write(
        "repo/.claude/rules/style.md",
        "---\npaths: src/**\n---\nrule",
    );
    f.write("repo/sub/AGENTS.md", "sub agents (overridden)");
    f.write("repo/sub/AGENTS.override.md", "sub override");
    f.write("repo/sub/CLAUDE.md", "sub claude");

    let project = ProjectHome::discover(repo.join("sub"));
    let (files, diagnostics) = project.instruction_files();
    let listed: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.path.strip_prefix(&repo).unwrap().to_path_buf(),
                file.kind,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (PathBuf::from("AGENTS.md"), InstructionKind::Agents),
            (
                PathBuf::from("CLAUDE.local.md"),
                InstructionKind::ClaudeLocal
            ),
            (
                PathBuf::from(".claude/CLAUDE.md"),
                InstructionKind::ClaudeDir
            ),
            (
                PathBuf::from(".claude/rules/style.md"),
                InstructionKind::Rule
            ),
            (
                PathBuf::from("sub/AGENTS.override.md"),
                InstructionKind::AgentsOverride
            ),
            (PathBuf::from("sub/CLAUDE.md"), InstructionKind::Claude),
        ]
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.path == repo.join("CLAUDE.md") && d.message.contains("same file"))
    );

    let read = project.read_instructions();
    assert_eq!(
        read.combined().as_deref(),
        Some("root agents\n\nroot local\n\nroot dot-claude\n\nsub override\n\nsub claude")
    );
}

#[test]
fn agent_profiles_union_project_first_and_keep_formats_apart() {
    let f = Fixture::new();
    f.write(
        ".claude/agents/reviewer.md",
        "---\nname: reviewer\n---\nuser",
    );
    f.write(".claude/agents/team/helper.md", "user helper");
    f.write(".codex/agents/reviewer.toml", "name = \"reviewer\"");
    let repo = f.base.join("repo");
    f.write("repo/.claude/agents/reviewer.md", "project");
    let project = ProjectHome::discover(&repo);
    let set = f.home.agent_profiles_for(&project);
    let got: Vec<_> = set
        .profiles
        .iter()
        .map(|p| (p.name.as_str(), p.root.scope, p.root.format))
        .collect();
    assert_eq!(
        got,
        [
            ("helper", Scope::User, ProfileFormat::ClaudeMarkdown),
            ("reviewer", Scope::Project, ProfileFormat::ClaudeMarkdown),
            ("reviewer", Scope::User, ProfileFormat::CodexToml),
        ]
    );
    assert_eq!(set.shadowed.len(), 1);
    assert_eq!(
        set.shadowed[0].path,
        f.base.join(".claude/agents/reviewer.md")
    );
}

#[cfg(unix)]
#[test]
fn linking_dry_run_apply_idempotency_and_resolution_agree() {
    let f = Fixture::new();
    f.write(".codex/AGENTS.md", "codex global");
    f.write(".codex/config.toml", "model = \"x\"");
    f.write(".codex/auth.json", "{}");
    f.skill(".codex/skills", "from-codex", "a");
    f.skill(".agents/skills", "from-agents", "b");
    f.write(".claude/settings.json", "{}");
    f.skill(".claude/skills", "from-claude", "c");

    // Dry run plans everything and changes nothing.
    let before = f.snapshot();
    let plan = f.home.link_natural_paths(LinkMode::DryRun);
    assert_eq!(f.snapshot(), before);
    let planned: Vec<_> = plan
        .changes()
        .map(|a| a.link.strip_prefix(&f.base).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        planned,
        [
            PathBuf::from(".claude/CLAUDE.md"),
            PathBuf::from(".codex/skills/from-agents"),
            PathBuf::from(".claude/skills/from-agents"),
            PathBuf::from(".codex/skills/from-claude"),
            PathBuf::from(".claude/skills/from-codex"),
        ]
    );
    assert!(
        plan.changes()
            .all(|a| a.outcome == LinkOutcome::WouldCreate)
    );
    assert!(plan.directories.is_empty());

    // Apply creates exactly the planned links, pointing at canonical sources.
    let applied = f.home.link_natural_paths(LinkMode::Apply);
    assert_eq!(applied.changes().count(), 5);
    assert!(applied.changes().all(|a| a.outcome == LinkOutcome::Created));
    assert_eq!(
        fs::read_to_string(f.base.join(".claude/CLAUDE.md")).unwrap(),
        "codex global"
    );
    assert_eq!(
        fs::read_link(f.base.join(".claude/CLAUDE.md")).unwrap(),
        f.base.join(".codex/AGENTS.md")
    );
    assert_eq!(
        fs::read_link(f.base.join(".codex/skills/from-agents")).unwrap(),
        f.base.join(".agents/skills/from-agents")
    );
    assert!(f.base.join(".claude/skills/from-codex/SKILL.md").is_file());
    // ~/.agents/skills is a source only; native-owned files are untouched and unlinked.
    assert!(!f.base.join(".agents/skills/from-codex").exists());
    for owned in [
        ".claude/config.toml",
        ".claude/auth.json",
        ".codex/settings.json",
    ] {
        assert!(
            fs::symlink_metadata(f.base.join(owned)).is_err(),
            "{owned} must not be linked"
        );
    }
    assert_eq!(
        fs::read_to_string(f.base.join(".codex/config.toml")).unwrap(),
        "model = \"x\""
    );

    // Idempotent: nothing more to do, everything reported as already linked.
    let after = f.snapshot();
    let again = f.home.link_natural_paths(LinkMode::Apply);
    assert_eq!(f.snapshot(), after);
    assert_eq!(again.changes().count(), 0);
    assert_eq!(again.problems().count(), 0);
    assert_eq!(
        again
            .actions
            .iter()
            .filter(|a| a.outcome == LinkOutcome::AlreadyLinked)
            .count(),
        5
    );

    // Union resolution sees each shared item exactly once through either path.
    assert_eq!(f.home.global_instructions().sources.len(), 1);
    let names: Vec<_> = f.home.skills().skills.into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["from-agents", "from-claude", "from-codex"]);
    assert!(f.home.skills().shadowed.is_empty());
}

#[cfg(unix)]
#[test]
fn linking_never_overwrites_and_reports_conflicts_and_dangling_links() {
    let f = Fixture::new();
    f.write(".codex/AGENTS.md", "codex");
    f.write(".claude/CLAUDE.md", "claude");
    f.skill(".codex/skills", "both", "codex copy");
    f.skill(".claude/skills", "both", "claude copy");
    f.skill(".codex/skills", "stale", "s");
    f.symlink(&f.base.join("gone"), ".claude/skills/stale");
    f.skill(".codex/skills", "blocked", "b");
    f.write(".claude/skills/blocked", "a file, not a skill");

    let before = f.snapshot();
    let report = f.home.link_natural_paths(LinkMode::Apply);
    assert_eq!(
        f.snapshot(),
        before,
        "nothing may be created, replaced or removed"
    );
    let outcome = |item: SharedItem| {
        report
            .actions
            .iter()
            .find(|a| a.item == item)
            .map(|a| a.outcome.clone())
            .unwrap()
    };
    assert_eq!(
        outcome(SharedItem::GlobalInstructions),
        LinkOutcome::BothExist
    );
    assert_eq!(
        outcome(SharedItem::Skill {
            name: "both".into()
        }),
        LinkOutcome::BothExist
    );
    assert_eq!(
        outcome(SharedItem::Skill {
            name: "stale".into()
        }),
        LinkOutcome::DanglingLink
    );
    assert!(matches!(
        outcome(SharedItem::Skill {
            name: "blocked".into()
        }),
        LinkOutcome::Blocked { .. }
    ));
    assert_eq!(report.problems().count(), 2);
    // Union resolution still exposes both global files.
    assert_eq!(
        f.home.global_instructions().combined().as_deref(),
        Some("codex\n\nclaude")
    );
}

#[cfg(unix)]
#[test]
fn linking_fills_codex_side_and_creates_only_needed_directories() {
    let f = Fixture::new();
    f.write(".claude/CLAUDE.md", "claude only");
    f.skill(".claude/skills", "mine", "m");

    let plan = f.home.link_natural_paths(LinkMode::DryRun);
    assert_eq!(
        plan.directories,
        [f.base.join(".codex"), f.base.join(".codex/skills")]
    );
    assert!(!f.base.join(".codex").exists());

    let report = f.home.link_natural_paths(LinkMode::Apply);
    assert_eq!(report.directories, plan.directories);
    assert_eq!(
        fs::read_link(f.base.join(".codex/AGENTS.md")).unwrap(),
        f.base.join(".claude/CLAUDE.md")
    );
    assert_eq!(
        fs::read_link(f.base.join(".codex/skills/mine")).unwrap(),
        f.base.join(".claude/skills/mine")
    );
    // Codex-side global resolution now reads the Claude file through its natural path, once.
    let global = f.home.global_instructions();
    assert_eq!(global.combined().as_deref(), Some("claude only"));
    assert_eq!(global.sources[0].path, f.base.join(".codex/AGENTS.md"));

    // A lone AGENTS.override.md is a Codex source: CLAUDE.md is not linked over it.
    let g = Fixture::new();
    g.write(".codex/AGENTS.override.md", "temp");
    let report = g.home.link_natural_paths(LinkMode::Apply);
    assert_eq!(
        fs::read_link(g.base.join(".claude/CLAUDE.md")).unwrap(),
        g.base.join(".codex/AGENTS.override.md")
    );
    assert!(!g.base.join(".codex/AGENTS.md").exists());
    assert_eq!(report.changes().count(), 1);
}

#[cfg(not(unix))]
#[test]
fn linking_is_reported_unsupported_and_changes_nothing_off_unix() {
    let f = Fixture::new();
    f.write(".codex/AGENTS.md", "codex");
    f.skill(".codex/skills", "only-codex", "s");
    let before = f.snapshot();
    for mode in [LinkMode::DryRun, LinkMode::Apply] {
        let report = f.home.link_natural_paths(mode);
        assert_eq!(f.snapshot(), before);
        assert!(report.directories.is_empty());
        for item in [
            SharedItem::GlobalInstructions,
            SharedItem::Skill {
                name: "only-codex".into(),
            },
        ] {
            assert!(
                report
                    .actions
                    .iter()
                    .any(|a| a.item == item && a.outcome == LinkOutcome::Unsupported),
                "{report:?}"
            );
        }
    }
}
