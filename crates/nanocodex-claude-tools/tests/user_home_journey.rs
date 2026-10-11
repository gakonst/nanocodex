//! User-home journeys: real temporary Codex/Claude homes and a project, resolved
//! through `nanocodex-home` and consumed by the Claude-native catalogs.
#![cfg(unix)]
use nanocodex_claude_tools::{
    ClaudeAgentProfiles, ClaudeProjectContext, ClaudeSkills, SkillInvocation,
};
use nanocodex_home::AgentHome;
use std::{fs, path::Path};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn user_home_skills_profiles_and_instructions_join_project_catalogs() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let (codex, claude, user, project) = (
        base.join("codex"),
        base.join("claude"),
        base.join("user"),
        base.join("project"),
    );
    write(&codex, "AGENTS.md", "Global Codex guidance.");
    write(&claude, "CLAUDE.md", "Global Claude guidance.");
    write(&project, "CLAUDE.md", "Project guidance.");
    write(
        &codex,
        "skills/shared/SKILL.md",
        "---\ndescription: personal shared skill\n---\nPersonal body $ARGUMENTS\n",
    );
    // A natural-path link from the Claude home to a Codex-home skill.
    fs::create_dir_all(claude.join("skills")).unwrap();
    std::os::unix::fs::symlink(codex.join("skills/shared"), claude.join("skills/linked")).unwrap();
    write(
        &project,
        ".claude/skills/shared/SKILL.md",
        "---\ndescription: project shared skill\n---\nProject body\n",
    );
    write(
        &project,
        ".claude/skills/local/SKILL.md",
        "---\ndescription: project-only skill\n---\nLocal body\n",
    );
    write(
        &claude,
        "agents/reviewer.md",
        "---\nname: reviewer\ndescription: personal reviewer\n---\nPersonal\n",
    );
    write(
        &claude,
        "agents/helper.md",
        "---\nname: helper\ndescription: personal helper\n---\nHelp\n",
    );
    write(&codex, "agents/reviewer.toml", "name = \"reviewer\"\n");
    write(
        &project,
        ".claude/agents/reviewer.md",
        "---\nname: reviewer\ndescription: project reviewer\n---\nProject\n",
    );
    let home = AgentHome::new(&codex, &claude).with_user_home(Some(user));

    let skills = ClaudeSkills::new(&project)
        .unwrap()
        .with_user_roots(home.skill_roots());
    let catalog = skills.catalog(SkillInvocation::Model);
    let described = catalog
        .skills
        .iter()
        .map(|skill| (skill.name.as_str(), skill.description.as_str()))
        .collect::<Vec<_>>();
    assert!(
        described.contains(&("shared", "personal shared skill")),
        "{catalog:?}"
    );
    assert!(
        described.contains(&("local", "project-only skill")),
        "{catalog:?}"
    );
    let expansion = skills
        .invoke("shared", "now", SkillInvocation::Model)
        .unwrap();
    assert_eq!(expansion.instructions.trim(), "Personal body now");
    assert!(Path::new(&expansion.base_directory).starts_with(&codex));
    // Workspace skills still never follow symlinks.
    std::os::unix::fs::symlink(
        codex.join("skills/shared"),
        project.join(".claude/skills/escaped"),
    )
    .unwrap();
    let names = skills
        .catalog(SkillInvocation::Model)
        .skills
        .into_iter()
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    assert!(!names.contains(&"escaped".to_owned()), "{names:?}");

    let profiles = ClaudeAgentProfiles::new(&project)
        .unwrap()
        .with_user_roots(home.agent_profile_roots())
        .catalog();
    let profiles = profiles
        .profiles
        .iter()
        .map(|profile| (profile.name.as_str(), profile.description.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        profiles,
        [
            ("helper", "personal helper"),
            ("reviewer", "project reviewer")
        ]
    );

    let context = ClaudeProjectContext::new(&project)
        .unwrap()
        .with_global_instructions(home.global_instructions())
        .load();
    let texts = context
        .excerpts
        .iter()
        .map(|excerpt| excerpt.text.trim())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            "Global Codex guidance.",
            "Global Claude guidance.",
            "Project guidance."
        ],
        "{context:?}"
    );
}
