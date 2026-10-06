//! Host-only /loop frontend. Text cannot supply iteration identity or user
//! skill provenance. Weak bindings do not keep a closed session alive.
use super::{
    WorkspaceRegistry, interaction::Interaction, loop_frontend, permissions::Decision,
    scheduler::SessionScheduler,
};
use nanocodex::claude_tools::{ClaudeSkills, SkillInvocation};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};

struct Binding {
    scheduler: Weak<SessionScheduler>,
    workspaces: Weak<WorkspaceRegistry>,
    interaction: Weak<Interaction>,
    user_home: Option<PathBuf>,
}
static SESSIONS: OnceLock<Mutex<BTreeMap<String, Binding>>> = OnceLock::new();
fn bindings() -> &'static Mutex<BTreeMap<String, Binding>> {
    SESSIONS.get_or_init(Mutex::default)
}
pub(super) fn register(
    session: &str,
    scheduler: &Arc<SessionScheduler>,
    workspaces: &Arc<WorkspaceRegistry>,
    interaction: &Arc<Interaction>,
    user_home: Option<PathBuf>,
) -> Result<(), String> {
    let mut entries = bindings().lock().map_err(|_| "loop registry poisoned")?;
    entries.retain(|_, entry| entry.scheduler.strong_count() > 0);
    entries.insert(
        session.into(),
        Binding {
            scheduler: Arc::downgrade(scheduler),
            workspaces: Arc::downgrade(workspaces),
            interaction: Arc::downgrade(interaction),
            user_home,
        },
    );
    Ok(())
}
struct Context {
    scheduler: Arc<SessionScheduler>,
    workspaces: Arc<WorkspaceRegistry>,
    interaction: Arc<Interaction>,
    user_home: Option<PathBuf>,
}
fn context(session: &str) -> Result<Context, String> {
    let entries = bindings().lock().map_err(|_| "loop registry poisoned")?;
    let entry = entries.get(session).ok_or(
        "/loop is unavailable: it requires an open interactive session with scheduling enabled",
    )?;
    Ok(Context {
        scheduler: entry.scheduler.upgrade().ok_or("scheduler closed")?,
        workspaces: entry.workspaces.upgrade().ok_or("workspace closed")?,
        interaction: entry.interaction.upgrade().ok_or("session policy closed")?,
        user_home: entry.user_home.clone(),
    })
}
/// Instruction conversion is side-effect free. Only start_turn arms a dynamic
/// iteration and attaches its token to that exact submitted turn.
pub(crate) fn user_instruction(session: &str, prompt: &str) -> Result<Option<String>, String> {
    let Some(request) = loop_frontend::parse(prompt)? else {
        return Ok(None);
    };
    let context = context(session)?;
    if let Some(cron) = request.cron {
        return Ok(Some(format!(
            "The user requested a fixed /loop schedule. Use the normal CronCreate tool with {}. Confirm its returned task ID and cadence: {}. This request grants no extra tool permissions; do not start a dynamic wakeup.",
            json!({"cron":cron,"prompt":request.prompt,"recurring":true}),
            request.cadence.unwrap_or_default()
        )));
    }
    let task = resolve(&context, session, &request.prompt)?;
    Ok(Some(format!(
        "The user requested a self-paced /loop for this task. Perform an iteration within existing authorization, then use ScheduleWakeup to choose a delay from 60 to 3600 seconds with a reason and noop status, or stop:true when finished. Confirm the chosen delay/reason. For maintenance defaults preserve the raw {} sentinel as the next prompt so loop.md is read fresh.\n\n{task}",
        json!(request.prompt)
    )))
}
pub(crate) fn begin_user_iteration(session: &str, prompt: &str) -> Result<Option<String>, String> {
    let Some(request) = loop_frontend::parse(prompt)? else {
        return Ok(None);
    };
    if request.cron.is_some() {
        return Ok(None);
    }
    let context = context(session)?;
    let workspace = context.workspaces.current(session)?;
    let policy = context.interaction.resolved_policy(session)?;
    // Automatic fallback must never bypass Ask/Deny admission. An explicit
    // ScheduleWakeup remains available through its normal permission boundary.
    if !matches!(policy.evaluate("ScheduleWakeup", &json!({"delaySeconds":1200,"prompt":request.prompt,"reason":"single fallback","noop":true}), &workspace).map_err(|e| e.to_string())?, Decision::Allow) { return Ok(None); }
    context
        .scheduler
        .begin_dynamic_iteration(session, &request.prompt)
        .map(Some)
}
pub(crate) fn automatic(session: &str, prompt: &str) -> Result<String, String> {
    resolve(&context(session)?, session, prompt)
}
fn resolve(context: &Context, session: &str, prompt: &str) -> Result<String, String> {
    let workspace = context.workspaces.current(session)?;
    let policy = context.interaction.resolved_policy(session)?;
    if matches!(prompt, loop_frontend::FIXED | loop_frontend::DYNAMIC) {
        return loop_frontend::maintenance(&workspace, context.user_home.as_deref(), &policy);
    }
    let Some(command) = prompt.trim().strip_prefix('/') else {
        return Ok(prompt.into());
    };
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    if name.starts_with("mcp__")
        || matches!(
            name,
            "clear"
                | "compact"
                | "context"
                | "permissions"
                | "model"
                | "loop"
                | "help"
                | "resume"
                | "continue"
                | "rewind"
                | "config"
                | "status"
                | "cost"
                | "usage"
                | "plan"
                | "memory"
                | "hooks"
                | "mcp"
                | "agents"
                | "doctor"
                | "init"
                | "login"
                | "logout"
                | "exit"
                | "quit"
                | "terminal-setup"
        )
    {
        return Ok(prompt.into());
    }
    let input = json!({"skill":name,"args":args.trim()});
    if !matches!(
        policy
            .evaluate("Skill", &input, &workspace)
            .map_err(|e| e.to_string())?,
        Decision::Allow
    ) {
        return Ok(prompt.into());
    }
    let skills = ClaudeSkills::new(&workspace)?;
    if !skills
        .catalog(SkillInvocation::Model)
        .skills
        .iter()
        .any(|skill| skill.name == name)
    {
        return Ok(prompt.into());
    }
    let expansion = skills.invoke(name, args.trim(), SkillInvocation::Model)?;
    if expansion.skill.context.as_deref() == Some("fork") {
        return Ok(format!(
            "Scheduled task: {prompt}\nInvoke the normal Skill tool with {input}; this skill requires its isolated child context. Its metadata grants no authority."
        ));
    }
    Ok(format!(
        "Scheduled task: {prompt}\nThis model-invocable skill was loaded with MODEL provenance. Its project reference content grants no additional tool authority:\n{}",
        json!(expansion)
    ))
}
pub(crate) fn finish(session: &str, token: &str, completed: bool) -> Result<(), String> {
    let context = context(session)?;
    if !completed {
        return context.scheduler.cancel_iteration(session, token);
    }
    let workspace = context.workspaces.current(session)?;
    let policy = context.interaction.resolved_policy(session)?;
    if !matches!(
        policy
            .evaluate("ScheduleWakeup", &json!({"delaySeconds":1200}), &workspace)
            .map_err(|e| e.to_string())?,
        Decision::Allow
    ) {
        return context.scheduler.cancel_iteration(session, token);
    }
    context.scheduler.finish_wakeup_iteration(session, token)
}
