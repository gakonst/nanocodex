//! Public SDK journeys with actual temporary project files and tool dispatch.
use nanocodex_claude_tools::{
    ClaudeProjectContext, ClaudeSkills, ClaudeWorkspaceFiles, SkillInvocation,
};
use serde_json::json;
use std::{fs, path::Path};
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
#[tokio::test]
async fn skill_catalog_invocation_edits_and_errors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        ".claude/skills/review/SKILL.md",
        "---\nname: review\ndescription: >-\n  Review a selected component.\nargument-hint: '[component] [mode]'\nallowed-tools: [Read, Grep]\n---\nReview $0 in $ARGUMENTS[1] mode. Full: $ARGUMENTS\n",
    );
    write(
        root,
        ".claude/skills/deploy/SKILL.md",
        "---\ndescription: User-only deployment procedure\ndisable-model-invocation: true\n---\nInspect $ARGUMENTS\n",
    );
    write(
        root,
        ".claude/skills/internal/SKILL.md",
        "---\nuser-invocable: false\n---\nInternal helper\n",
    );
    write(
        root,
        ".claude/skills/broken/SKILL.md",
        "---\ndisable-model-invocation: perhaps\n---\nInvalid\n",
    );
    write(
        root,
        ".claude/skills/forked/SKILL.md",
        "---\ncontext: fork\n---\nChild execution required\n",
    );
    write(
        root,
        ".claude/skills/hooks/SKILL.md",
        "---\nhooks: {}\n---\nUnsupported execution\n",
    );
    write(
        root,
        ".claude/skills/inject/SKILL.md",
        "Never execute !`touch unexpected`\n",
    );
    let skills = ClaudeSkills::new(root).unwrap();
    let model = skills.catalog(SkillInvocation::Model);
    println!("model_catalog={}", json!(model));
    assert!(
        model
            .skills
            .iter()
            .any(|s| s.name == "review" && s.description == "Review a selected component.")
    );
    assert!(!model.skills.iter().any(|s| s.name == "deploy"));
    assert!(model.skills.iter().any(|s| s.name == "internal"));
    assert!(
        model
            .diagnostics
            .iter()
            .any(|s| s.contains("must be a boolean"))
    );
    assert!(
        model
            .diagnostics
            .iter()
            .any(|s| s.contains("unsupported execution frontmatter"))
    );
    assert!(
        model
            .skills
            .iter()
            .any(|s| s.name == "forked" && s.context.as_deref() == Some("fork"))
    );
    let fork_error = skills
        .execute("Skill", json!({"skill":"forked"}))
        .await
        .unwrap_err();
    assert!(
        fork_error.contains("host-owned child executor"),
        "{fork_error}"
    );
    let raw = skills
        .execute(
            "Skill",
            json!({"skill":"review","args":"\"core api\" strict"}),
        )
        .await
        .unwrap();
    println!("Skill success={raw}");
    let expanded: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        expanded["instructions"],
        "Review core api in strict mode. Full: \"core api\" strict\n"
    );
    assert_eq!(expanded["skill"]["allowed_tools"], json!(["Read", "Grep"]));
    for input in [
        json!({"skill":"deploy"}),
        json!({"skill":"review","caller":"user"}),
        json!({"skill":"../review"}),
        json!({"skill":"missing"}),
        json!({"skill":"review","args":"'unterminated"}),
        json!({"skill":"inject"}),
    ] {
        let error = skills.execute("Skill", input.clone()).await.unwrap_err();
        println!("Skill error input={input} error={error}");
    }
    assert!(!root.join("unexpected").exists());
    let explicit = skills
        .invoke("deploy", "release", SkillInvocation::User)
        .unwrap();
    assert!(explicit.instructions.contains("Inspect release"));
    // Replacement strings are literal and do not recursively expand placeholders.
    assert!(
        skills
            .invoke("review", "'$ARGUMENTS' '$0'", SkillInvocation::Model)
            .unwrap()
            .instructions
            .starts_with("Review $ARGUMENTS in $0 mode")
    );
    write(
        root,
        ".claude/skills/review/SKILL.md",
        "---\ndisable-model-invocation: true\n---\nUpdated\n",
    );
    assert!(
        skills
            .execute("Skill", json!({"skill":"review"}))
            .await
            .is_err()
    );
    assert_eq!(
        skills
            .invoke("review", "", SkillInvocation::User)
            .unwrap()
            .instructions,
        "Updated\n"
    );
    write(
        root,
        ".claude/settings.json",
        r#"{"skillOverrides":{"review":"off","internal":"off"}}"#,
    );
    assert!(
        skills
            .invoke("review", "", SkillInvocation::User)
            .unwrap_err()
            .contains("skillOverrides")
    );
    write(
        root,
        ".claude/settings.local.json",
        r#"{"skillOverrides":{"review":"name-only","internal":"user-invocable-only"}}"#,
    );
    let visible = skills.catalog(SkillInvocation::User);
    assert!(
        visible
            .skills
            .iter()
            .any(|s| s.name == "review" && s.description.is_empty())
    );
    // A local override does not elevate the skill's own user-invocable:false.
    assert!(!visible.skills.iter().any(|s| s.name == "internal"));
    assert!(
        skills
            .invoke("internal", "", SkillInvocation::Model)
            .unwrap_err()
            .contains("skillOverrides")
    );
    write(
        root,
        ".claude/settings.local.json",
        r#"{"skillOverrides":{"review":"invalid"}}"#,
    );
    assert!(skills.catalog(SkillInvocation::Model).skills.is_empty());
    assert!(
        skills
            .invoke("review", "", SkillInvocation::User)
            .unwrap_err()
            .contains("skillOverrides")
    );
    write(root, ".claude/settings.local.json", &" ".repeat(32769));
    let oversized = skills.catalog(SkillInvocation::Model);
    assert!(oversized.skills.is_empty());
    assert!(
        oversized
            .diagnostics
            .iter()
            .any(|d| d.contains("exceeds 32 KiB"))
    );
    println!("skillOverrides oversized settings={}", json!(oversized));
    fs::remove_file(root.join(".claude/settings.local.json")).unwrap();
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("settings.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("settings.json"),
            root.join(".claude/settings.local.json"),
        )
        .unwrap();
        let linked = skills.catalog(SkillInvocation::Model);
        assert!(linked.skills.is_empty());
        assert!(
            linked
                .diagnostics
                .iter()
                .any(|d| d.contains("skillOverrides .claude/settings.local.json"))
        );
        assert!(skills.invoke("review", "", SkillInvocation::User).is_err());
        println!("skillOverrides symlink settings={}", json!(linked));
        fs::remove_file(root.join(".claude/settings.local.json")).unwrap();
    }
    fs::remove_file(root.join(".claude/settings.json")).unwrap();
    fs::remove_file(root.join(".claude/skills/review/SKILL.md")).unwrap();
    assert!(skills.invoke("review", "", SkillInvocation::User).is_err());
    println!("live_edit_and_removal=observed; shell_side_effect=absent");
}
#[test]
fn scoped_context_imports_and_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "CLAUDE.md",
        "Root guidance\nSee @docs/team.md and `@inline-code.md`.\n@../outside.md\n```\n@not-an-import.md\n~~~\n@still-code.md\n```\n",
    );
    write(root, "CLAUDE.local.md", "Local guidance\n");
    write(
        root,
        "docs/team.md",
        "Imported guidance\n@loop.md\n@../shared.md\n",
    );
    write(root, "docs/loop.md", "Loop guidance\n@team.md\n");
    write(root, "shared.md", "Parent import remains inside root\n");
    write(root, "src/CLAUDE.md", "Nested guidance\n");
    write(root, "src/main.rs", "fn main() {}\n");
    write(root, "web/main.ts", "export {};\n");
    write(
        root,
        ".claude/rules/rust.md",
        "---\npaths:\n  - 'src/**/*.rs'\n---\nRust-specific guidance\n",
    );
    write(root, ".claude/rules/common.md", "Common guidance\n");
    write(
        root,
        ".claude/rules/web.md",
        "---\npaths: ['web/**/*.ts']\n---\nWeb-specific guidance\n",
    );
    let loader = ClaudeProjectContext::new(root).unwrap();
    let context = loader.load_for_path("src/main.rs");
    println!("scoped_context={}", json!(context));
    let all = context
        .excerpts
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "Root guidance",
        "Local guidance",
        "Imported guidance",
        "Loop guidance",
        "Parent import remains inside root",
        "Nested guidance",
        "Rust-specific guidance",
        "Common guidance",
    ] {
        assert!(all.contains(expected), "missing {expected}");
    }
    assert!(!all.contains("Web-specific guidance"));
    assert_eq!(
        context
            .excerpts
            .iter()
            .filter(|e| e.path == "docs/team.md")
            .count(),
        1
    );
    assert!(
        context
            .diagnostics
            .iter()
            .any(|s| s.contains("outside permitted"))
    );
    assert!(
        !context
            .diagnostics
            .iter()
            .any(|s| s.contains("not-an-import")
                || s.contains("inline-code")
                || s.contains("still-code"))
    );
    let startup = loader.load();
    assert!(
        !startup
            .excerpts
            .iter()
            .any(|e| e.text.contains("Rust-specific"))
    );
    assert!(loader.load_for_path("../outside").excerpts.is_empty());
    write(root, "CLAUDE.local.md", &"x".repeat(40 * 1024));
    assert!(
        loader
            .load()
            .excerpts
            .iter()
            .any(|e| e.path == "CLAUDE.local.md" && e.truncated)
    );
    println!("context_traversal=denied; cycles=deduplicated; oversized_excerpt=marked_truncated");
}
#[cfg(unix)]
#[tokio::test]
async fn symlinked_context_and_skills_do_not_escape() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.md", "SECRET_SHOULD_NOT_APPEAR");
    write(root.path(), ".claude/skills/escape/placeholder", "");
    symlink(
        outside.path().join("secret.md"),
        root.path().join("CLAUDE.md"),
    )
    .unwrap();
    symlink(
        outside.path().join("secret.md"),
        root.path().join(".claude/skills/escape/SKILL.md"),
    )
    .unwrap();
    let context = ClaudeProjectContext::new(root.path()).unwrap().load();
    assert!(context.excerpts.is_empty());
    assert!(!context.diagnostics.is_empty());
    let skills = ClaudeSkills::new(root.path()).unwrap();
    let result = skills.execute("Skill", json!({"skill":"escape"})).await;
    assert!(result.is_err());
    println!(
        "symlink_context={}; symlink_skill={:?}",
        json!(context),
        result
    );
}

/// Exercises the asynchronous context transport and resource failures from the
/// same public APIs that the CLI installs, using real files (no adapter mocks).
#[tokio::test]
async fn context_tool_refresh_and_bounded_skill_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "CLAUDE.md", "Initial root guidance\n");
    write(root, "src/CLAUDE.local.md", "Private nested guidance\n");
    write(root, "src/main.rs", "fn main() {}\n");
    let loader = ClaudeProjectContext::new(root).unwrap();
    let first = loader
        .execute("ProjectContext", json!({"path":"src/main.rs"}))
        .await
        .unwrap();
    assert!(first.contains("Private nested guidance"));
    write(root, "src/CLAUDE.local.md", "Updated nested guidance\n");
    let refreshed = loader
        .execute("ProjectContext", json!({"path":"src/main.rs"}))
        .await
        .unwrap();
    assert!(refreshed.contains("Updated nested guidance"));
    assert!(!refreshed.contains("Private nested guidance"));
    println!("ProjectContext refresh={refreshed}");
    for input in [
        json!({"path":"../outside"}),
        json!({"path":"/etc/passwd"}),
        json!({"path":23}),
        json!({"path":"src/main.rs","root":"/"}),
    ] {
        let error = loader
            .execute("ProjectContext", input.clone())
            .await
            .unwrap_err();
        println!("ProjectContext error input={input} error={error}");
    }
    write(
        root,
        ".claude/skills/large/SKILL.md",
        &"$ARGUMENTS\n".repeat(2000),
    );
    let skills = ClaudeSkills::new(root).unwrap();
    let error = skills
        .execute("Skill", json!({"skill":"large","args":"x".repeat(8192)}))
        .await
        .unwrap_err();
    assert!(error.contains("expanded skill exceeds 64 KiB"), "{error}");
    println!("Skill expansion bound={error}");
    let error = skills
        .execute("Skill", json!({"skill":"large","args":"x".repeat(8193)}))
        .await
        .unwrap_err();
    assert!(error.contains("arguments exceed 8 KiB"), "{error}");
    println!("Skill argument bound={error}");
    write(root, ".claude/skills/large/SKILL.md", &"x".repeat(32769));
    let catalog = skills.catalog(SkillInvocation::Model);
    assert!(catalog.skills.is_empty());
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|d| d.contains("exceeds 32 KiB"))
    );
    println!("Skill file bound={}", json!(catalog));
    // A corrected file immediately recovers without restarting the adapter.
    write(
        root,
        ".claude/skills/large/SKILL.md",
        "Inspect the supplied component.",
    );
    let recovered = skills
        .execute("Skill", json!({"skill":"large","args":"core"}))
        .await
        .unwrap();
    assert!(recovered.contains("ARGUMENTS: core"));
    println!("Skill recovery={recovered}");
}

#[tokio::test]
async fn file_operations_automatically_deliver_scoped_guidance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "CLAUDE.md",
        "ROOT_AUTOMATIC_GUIDANCE\n@docs/shared.md\n",
    );
    write(root, "docs/shared.md", "SHARED_AUTOMATIC_GUIDANCE\n");
    write(root, "src/CLAUDE.md", "NESTED_AUTOMATIC_GUIDANCE\n");
    write(root, "src/main.rs", "fn main() {}\n");
    write(
        root,
        ".claude/rules/rust.md",
        "---\npaths: ['src/**/*.rs']\n---\nRUST_AUTOMATIC_GUIDANCE\n",
    );
    write(
        root,
        ".claude/rules/web.md",
        "---\npaths: ['web/**/*.ts']\n---\nWEB_AUTOMATIC_GUIDANCE\n",
    );
    let files = ClaudeWorkspaceFiles::new(root).unwrap();
    for (name, input) in [
        ("Read", json!({"file_path":"src/main.rs"})),
        (
            "Edit",
            json!({"file_path":"src/main.rs","old_string":"fn main() {}","new_string":"fn main() { println!(\"done\"); }"}),
        ),
        (
            "Write",
            json!({"file_path":"src/new.rs","content":"// written\n"}),
        ),
        ("Grep", json!({"path":"src/main.rs","pattern":"main"})),
        ("Glob", json!({"path":"src","pattern":"*.rs"})),
    ] {
        let output = files.execute_output(name, input).await.unwrap();
        let metadata = output.metadata.unwrap();
        let excerpts = metadata["project_context"]["excerpts"].as_array().unwrap();
        let all = excerpts
            .iter()
            .map(|e| e["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for marker in [
            "ROOT_AUTOMATIC_GUIDANCE",
            "SHARED_AUTOMATIC_GUIDANCE",
            "NESTED_AUTOMATIC_GUIDANCE",
        ] {
            assert!(all.contains(marker), "{name}: missing {marker}: {all}");
        }
        if name != "Glob" {
            assert!(all.contains("RUST_AUTOMATIC_GUIDANCE"), "{name}: {all}");
        }
        assert!(!all.contains("WEB_AUTOMATIC_GUIDANCE"), "{name}: {all}");
        assert_eq!(
            excerpts
                .iter()
                .filter(|e| e["path"] == "docs/shared.md")
                .count(),
            1
        );
        println!("automatic_context tool={name} metadata={metadata}");
    }
    assert!(
        fs::read_to_string(root.join("src/main.rs"))
            .unwrap()
            .contains("done")
    );
    assert_eq!(
        fs::read_to_string(root.join("src/new.rs")).unwrap(),
        "// written\n"
    );
    write(root, "src/CLAUDE.md", "UPDATED_AUTOMATIC_GUIDANCE\n");
    let refreshed = files
        .execute("Read", json!({"file_path":"src/main.rs"}))
        .await
        .unwrap();
    assert!(refreshed.contains("UPDATED_AUTOMATIC_GUIDANCE"));
    assert!(!refreshed.contains("NESTED_AUTOMATIC_GUIDANCE"));
    let isolated = files
        .execute_output_with_context("Read", json!({"file_path":"src/main.rs"}), false)
        .await
        .unwrap();
    assert!(isolated.metadata.is_none());
    let nanocodex_claude_tools::ToolContent::Text(body) = isolated.content else {
        panic!("expected text Read result");
    };
    assert!(body.contains("println!"));
    assert!(!body.contains("AUTOMATIC_GUIDANCE"));
    println!("file_context_live_refresh=observed; effects=verified; optional_context=disabled");
}
