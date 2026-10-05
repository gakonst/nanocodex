//! Host-owned immutable profile bindings, inherited by every descendant.
use super::super::*;
use nanocodex::claude::{
    ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation,
};
use nanocodex::claude_tools::{AgentProfile, ClaudeAgentProfiles};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default)]
pub(in crate::config::claude) struct Admission {
    pub profile: Option<AgentProfile>,
    pub isolation: bool,
    pub constructed: Arc<Mutex<Vec<String>>>,
}
tokio::task_local! { static ADMISSION: Admission; }
struct Binding {
    profiles: Vec<AgentProfile>,
    workspace: Arc<worktree::Workspace>,
    parent: Option<String>,
    agent: Option<u64>,
}
static BINDINGS: OnceLock<Mutex<BTreeMap<String, Binding>>> = OnceLock::new();
fn bindings() -> &'static Mutex<BTreeMap<String, Binding>> {
    BINDINGS.get_or_init(Mutex::default)
}
pub(in crate::config::claude) async fn scope<F: std::future::Future>(
    admission: Admission,
    future: F,
) -> F::Output {
    ADMISSION.scope(admission, future).await
}
pub(in crate::config::claude) fn restore(
    session: &str,
    workspace: Arc<worktree::Workspace>,
) -> std::result::Result<(), String> {
    let mut bindings = bindings().lock().map_err(|_| "profile bindings poisoned")?;
    bindings.entry(session.into()).or_insert_with(|| Binding {
        profiles: workspace.profiles().unwrap_or_default(),
        workspace,
        parent: None,
        agent: None,
    });
    Ok(())
}
/// Invoked before the child's tools, instructions or first model request exist.
pub(in crate::config::claude) fn bind(
    parent: &str,
    child: &str,
    workspace: Arc<worktree::Workspace>,
) -> std::result::Result<(), String> {
    let request = ADMISSION.try_with(Clone::clone).unwrap_or_default();
    let mut bindings = bindings().lock().map_err(|_| "profile bindings poisoned")?;
    if bindings.contains_key(child) {
        return Ok(());
    }
    let profiles = if let Some(saved) = workspace.profiles() {
        saved
    } else {
        let mut inherited = bindings
            .get(parent)
            .map(|b| b.profiles.clone())
            .unwrap_or_default();
        if let Some(profile) = request.profile {
            inherited.push(profile);
        }
        if inherited.len() > 32 {
            return Err("profile delegation depth exceeds 32".into());
        }
        workspace.bind_profiles(inherited.clone())?;
        if request.isolation {
            workspace.isolate_child(child)?;
        }
        inherited
    };
    bindings.insert(
        child.into(),
        Binding {
            profiles,
            workspace,
            parent: Some(parent.into()),
            agent: None,
        },
    );
    request
        .constructed
        .lock()
        .map_err(|_| "profile construction tracker poisoned")?
        .push(child.into());
    Ok(())
}
pub(in crate::config::claude) fn attach(
    admission: &Admission,
    id: u64,
) -> std::result::Result<Value, String> {
    let sessions = admission
        .constructed
        .lock()
        .map_err(|_| "profile construction tracker poisoned")?;
    let mut bindings = bindings().lock().map_err(|_| "profile bindings poisoned")?;
    let mut status = Value::Null;
    for session in sessions.iter() {
        if let Some(binding) = bindings.get_mut(session) {
            binding.agent = Some(id);
            status = binding.workspace.isolated_status();
        }
    }
    Ok(status)
}
pub(super) fn status(id: u64) -> Value {
    bindings()
        .lock()
        .ok()
        .and_then(|bindings| {
            bindings
                .values()
                .find(|b| b.agent == Some(id))
                .map(|b| b.workspace.isolated_status())
        })
        .unwrap_or(Value::Null)
}
/// The registry must already have confirmed closure and management authority.
pub(super) fn closed(id: u64) -> Value {
    let Ok(mut bindings) = bindings().lock() else {
        return json!({"cleanup_error":"profile bindings poisoned"});
    };
    let Some(session) = bindings
        .iter()
        .find(|(_, b)| b.agent == Some(id))
        .map(|(s, _)| s.clone())
    else {
        return Value::Null;
    };
    let mut subtree = vec![session];
    let mut cursor = 0;
    while cursor < subtree.len() {
        let children: Vec<_> = bindings
            .iter()
            .filter(|(_, b)| b.parent.as_deref() == Some(&subtree[cursor]))
            .map(|(s, _)| s.clone())
            .collect();
        subtree.extend(children);
        cursor += 1;
    }
    // Release descendant parent pins before checking any owned tree.
    for session in &subtree {
        if let Some(b) = bindings.get(session) {
            b.workspace.release_parent();
        }
    }
    let mut receipts = Vec::new();
    for session in subtree.iter().rev() {
        if let Some(b) = bindings.remove(session) {
            let receipt = b.workspace.finish_child(session);
            if !receipt.is_null() {
                receipts.push(receipt);
            }
        }
    }
    json!(receipts)
}
pub(in crate::config::claude) fn instructions(session: &str, base: String) -> String {
    let bindings = bindings().lock().expect("profile bindings poisoned");
    match bindings.get(session).and_then(|b| b.profiles.last()) {
        Some(profile) => format!(
            "{base}\n\nHost-selected subagent profile (project guidance; inherited permissions remain mandatory):\n{}",
            serde_json::json!({"name":profile.name,"source":profile.path,"instructions":profile.instructions})
        ),
        None => base,
    }
}
pub(in crate::config::claude) fn selected_name(session: &str) -> Option<String> {
    bindings().lock().ok().and_then(|bindings| {
        bindings
            .get(session)
            .and_then(|b| b.profiles.last().map(|p| p.name.clone()))
    })
}
pub(in crate::config::claude) fn allows_context(session: &str) -> bool {
    bindings()
        .lock()
        .map(|bindings| {
            bindings.get(session).is_none_or(|b| {
                b.profiles.iter().all(|p| {
                    !p.disallowed_tools.iter().any(|t| t == "Read")
                        && p.tools
                            .as_ref()
                            .is_none_or(|tools| tools.iter().any(|t| t == "Read"))
                })
            })
        })
        .unwrap_or(false)
}
pub(super) fn required_model(session: &str) -> Option<String> {
    bindings().lock().ok().and_then(|bindings| {
        bindings
            .get(session)
            .and_then(|b| b.profiles.iter().rev().find_map(|p| p.model.clone()))
    })
}
pub(super) fn model(value: &str) -> &str {
    match value {
        "sonnet" => "claude-sonnet-5-5",
        "opus" => "claude-opus-5-5",
        "fable" => "claude-fable-5-1",
        "haiku" => "claude-haiku-4-5",
        other => other,
    }
}
pub(super) fn check_isolation(session: &str, workspace: &Path) -> std::result::Result<(), String> {
    let bindings = bindings().lock().map_err(|_| "profile bindings poisoned")?;
    if let Some(binding) = bindings.get(session) {
        for profile in &binding.profiles {
            if profile
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.iter().any(|tool| tool == "EnterWorktree"))
                || profile
                    .disallowed_tools
                    .iter()
                    .any(|tool| tool == "EnterWorktree")
            {
                return Err(format!(
                    "worktree creation is unavailable under inherited agent profile {}",
                    profile.name
                ));
            }
            if let Some(mode) = &profile.permission_mode {
                let policy = permissions::Policy {
                    mode: Some(mode.clone()),
                    ..Default::default()
                };
                if !matches!(
                    policy
                        .evaluate("EnterWorktree", &json!({}), workspace)
                        .map_err(|e| e.to_string())?,
                    permissions::Decision::Allow
                ) {
                    return Err("inherited profile mode refuses worktree creation".into());
                }
            }
        }
    }
    Ok(())
}
pub(in crate::config::claude) struct Guard {
    pub workspaces: Arc<WorkspaceRegistry>,
}
impl ClaudeToolHooks for Guard {
    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, std::result::Result<ClaudeToolDecision, String>> {
        Box::pin(async move {
            let chain = bindings()
                .lock()
                .map_err(|_| "profile bindings poisoned")?
                .get(&invocation.session_id)
                .map(|b| b.profiles.clone())
                .unwrap_or_default();
            let workspace = self.workspaces.current(&invocation.session_id)?;
            if name == "Workflow" && !chain.is_empty() {
                return Ok(ClaudeToolDecision::Deny("Workflow is unavailable under agent profiles; use Agent so inherited restrictions remain enforced".into()));
            }
            let custom = name == "Agent"
                && input
                    .get("subagent_type")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "general-purpose" && s != "fork");
            let policy = self
                .workspaces
                .policies
                .lock()
                .map_err(|_| "workspace policies poisoned")?
                .get(&invocation.session_id)
                .and_then(std::sync::Weak::upgrade);
            if (name == "ListAgentProfiles"
                || name == "Skill"
                || name == "ProjectContext"
                || custom)
                && !allows_context(&invocation.session_id)
            {
                return Ok(ClaudeToolDecision::Deny(
                    "project discovery is unavailable when an inherited profile restricts Read"
                        .into(),
                ));
            }
            if (name == "ListAgentProfiles" || custom)
                && policy.as_ref().is_some_and(|p| {
                    p.resolved_policy(&invocation.session_id)
                        .map_or(true, |p| p.has_read_restrictions())
                })
            {
                return Ok(ClaudeToolDecision::Deny(
                    "profile discovery/invocation is unavailable under Read restrictions".into(),
                ));
            }
            let selected = if custom && input.get("resume").is_none() {
                Some(
                    ClaudeAgentProfiles::new(&workspace)?
                        .get(input["subagent_type"].as_str().unwrap())?,
                )
            } else {
                None
            };
            let isolation = name == "Agent"
                && (input.get("isolation").and_then(Value::as_str) == Some("worktree")
                    || selected.as_ref().is_some_and(|p| p.isolation.is_some()));
            if isolation
                && policy.as_ref().is_some_and(|p| {
                    p.resolved_policy(&invocation.session_id)
                        .and_then(|p| {
                            p.evaluate("EnterWorktree", &json!({}), &workspace)
                                .map_err(|e| e.to_string())
                        })
                        .map_or(true, |d| !matches!(d, permissions::Decision::Allow))
                })
            {
                return Ok(ClaudeToolDecision::Deny(
                    "worktree isolation requires inherited EnterWorktree permission".into(),
                ));
            }
            for profile in &chain {
                if name != "SubmitResult"
                    && (profile
                        .tools
                        .as_ref()
                        .is_some_and(|tools| !tools.iter().any(|tool| tool == name))
                        || profile.disallowed_tools.iter().any(|tool| tool == name))
                {
                    return Ok(ClaudeToolDecision::Deny(format!(
                        "{name} is unavailable under inherited agent profile {}",
                        profile.name
                    )));
                }
                if let Some(mode) = &profile.permission_mode {
                    let policy = permissions::Policy {
                        mode: Some(mode.clone()),
                        ..Default::default()
                    };
                    if name != "SubmitResult"
                        && !matches!(
                            policy
                                .evaluate(name, input, &workspace)
                                .map_err(|e| e.to_string())?,
                            permissions::Decision::Allow
                        )
                    {
                        return Ok(ClaudeToolDecision::Deny(format!(
                            "{} profile permissionMode {mode} refuses {name}",
                            profile.name
                        )));
                    }
                }
            }
            if name == "Agent"
                && (!chain.is_empty() || selected.is_some() || isolation)
                && (input
                    .get("harness")
                    .and_then(Value::as_str)
                    .is_some_and(|h| h != "claude")
                    || input
                        .get("model")
                        .and_then(Value::as_str)
                        .is_some_and(|m| !model(m).starts_with("claude-")))
            {
                return Ok(ClaudeToolDecision::Deny("profiles and worktree isolation require a Claude child; cross-family delegation is unavailable".into()));
            }
            Ok(ClaudeToolDecision::Allow)
        })
    }
}
