//! Canonical Codex prompt composition from explicit host facts.
//!
//! Loading an asset does not enable its mode or authorize any operation. Callers
//! must select only modes and permission facts their runtime actually supports.

#[allow(missing_docs)]
#[path = "../prompts/codex/utils/template/src/lib.rs"]
pub mod template;

use crate::prompt_assets::asset;

/// Render a pinned asset using the upstream strict, non-recursive renderer.
pub fn render<'a>(
    path: &str,
    values: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<String, String> {
    let text = asset(path).ok_or_else(|| format!("unknown canonical prompt: {path}"))?;
    template::render(text, values).map_err(|error| error.to_string())
}

/// Resolve a model-owned message, preserving explicit empty catalog overrides.
pub fn model_message(model: &str, field: &str, fallback: &'static str) -> &'static str {
    asset(&format!("catalog/{model}/{field}.txt")).unwrap_or(fallback)
}

/// Actual filesystem enforcement selected by the execution host.
#[derive(Clone, Copy, Debug)]
pub enum SandboxMode {
    /// No filesystem restriction is enforced.
    DangerFullAccess,
    /// Writes are restricted to the supplied roots.
    WorkspaceWrite,
    /// Filesystem writes are forbidden.
    ReadOnly,
}

/// Actual approval policy enforced by the execution host.
#[derive(Clone, Copy, Debug)]
pub enum ApprovalPolicy {
    /// Escalation is rejected.
    Never,
    /// Escalation can be requested from the user.
    OnRequest,
    /// Untrusted commands require approval.
    UnlessTrusted,
}

/// Facts used to compose the canonical permissions fragment. No ambient grants
/// or credentials are inspected, inferred, or serialized by this renderer.
pub struct PermissionFacts<'a> {
    /// Current filesystem enforcement.
    pub sandbox: SandboxMode,
    /// Whether outbound networking is permitted.
    pub network_enabled: bool,
    /// Current approval enforcement.
    pub approval: ApprovalPolicy,
    /// Resolved writable roots in canonical display order.
    pub writable_roots: &'a [&'a str],
    /// Exact paths which remain unreadable even after escalation.
    pub denied_read_paths: &'a [&'a str],
    /// Glob restrictions which remain in force after escalation.
    pub denied_read_globs: &'a [&'a str],
}

/// Compose the bundled permissions text with the same section/whitespace rules
/// as Codex `PermissionsInstructions::from_resolved`. Advanced approval variants
/// must be implemented before being exposed by a host; they are not approximated.
pub fn permissions(facts: &PermissionFacts<'_>) -> String {
    let sandbox = match facts.sandbox {
        SandboxMode::DangerFullAccess => "danger_full_access",
        SandboxMode::WorkspaceWrite => "workspace_write",
        SandboxMode::ReadOnly => "read_only",
    };
    let approval = match facts.approval {
        ApprovalPolicy::Never => "never",
        ApprovalPolicy::OnRequest => "on_request",
        ApprovalPolicy::UnlessTrusted => "unless_trusted",
    };
    let source = asset(&format!(
        "prompts/templates/permissions/sandbox_mode/{sandbox}.md"
    ))
    .expect("pinned sandbox template");
    let sandbox = template::render(
        source.trim_end(),
        [(
            "network_access",
            if facts.network_enabled {
                "enabled"
            } else {
                "restricted"
            },
        )],
    )
    .expect("pinned sandbox interpolation");
    let mut body = String::new();
    append_section(&mut body, &sandbox);
    append_section(
        &mut body,
        asset(&format!(
            "prompts/templates/permissions/approval_policy/{approval}.md"
        ))
        .expect("pinned approval template"),
    );
    if !facts.writable_roots.is_empty() {
        let roots = facts
            .writable_roots
            .iter()
            .map(|root| format!("`{root}`"))
            .collect::<Vec<_>>();
        append_section(
            &mut body,
            &if roots.len() == 1 {
                format!(" The writable root is {}.", roots[0])
            } else {
                format!(" The writable roots are {}.", roots.join(", "))
            },
        );
    }
    let entries = facts
        .denied_read_paths
        .iter()
        .map(|path| format!("- path `{path}`"))
        .chain(
            facts
                .denied_read_globs
                .iter()
                .map(|glob| format!("- glob `{glob}`")),
        )
        .collect::<Vec<_>>();
    if !entries.is_empty() {
        append_section(
            &mut body,
            &format!(
                "## Denied filesystem reads\nThe active permission profile denies reading these paths/globs. Do not request escalation or additional permissions to read them; these denials are policy restrictions.\n{}",
                entries.join("\n")
            ),
        );
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    format!("<permissions instructions>{body}</permissions instructions>")
}

fn append_section(body: &mut String, section: &str) {
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(section);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_use_enforced_facts_and_exact_bundled_bytes() {
        let facts = PermissionFacts {
            sandbox: SandboxMode::DangerFullAccess,
            network_enabled: true,
            approval: ApprovalPolicy::Never,
            writable_roots: &[],
            denied_read_paths: &[],
            denied_read_globs: &[],
        };
        assert_eq!(
            permissions(&facts),
            "<permissions instructions>\nFilesystem sandboxing defines which files can be read or written. `sandbox_mode` is `danger-full-access`: No filesystem sandboxing - all commands are permitted. Network access is enabled.\nApproval policy is currently never. Do not provide the `sandbox_permissions` for any reason, commands will be rejected.\n</permissions instructions>"
        );
        let restricted = PermissionFacts {
            sandbox: SandboxMode::ReadOnly,
            network_enabled: false,
            denied_read_paths: &["/private"],
            ..facts
        };
        let text = permissions(&restricted);
        assert!(text.contains("`read-only`"));
        assert!(text.contains("Network access is restricted."));
        assert!(text.contains("- path `/private`"));
        assert!(!text.contains("all commands are permitted"));
    }

    #[test]
    fn strict_renderer_preserves_literal_user_text() {
        let rendered = render(
            "prompts/templates/review/exit_success.xml",
            [("results", "{{ untouched }} <x>\n")],
        )
        .unwrap();
        assert!(rendered.contains("{{ untouched }} <x>\n"));
        assert!(render("prompts/templates/review/exit_success.xml", []).is_err());
        assert!(render("not/a/prompt", []).is_err());
    }

    #[test]
    fn catalog_overrides_are_selected_without_rewriting() {
        let astra = model_message("gpt-6-astra", "persistent_instructions", "fallback");
        assert!(astra.starts_with("## Overview\n"));
        assert_eq!(
            model_message("gpt-5.6-sol", "persistent_instructions", "fallback"),
            "fallback"
        );
        assert!(crate::prompt_assets::ASSETS.len() > 60);
    }
}
