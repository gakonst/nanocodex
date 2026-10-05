//! Opt-in, bounded JavaScript orchestration over the native agent registry.
use super::*;
use nanocodex::{
    Tool, Tools,
    agent::AgentHandle,
    tools::contract::{ToolDefinition as HostDefinition, ToolOutput, ToolResult, async_trait},
};
use nanocodex_subagents::{AgentId, AgentStatus, AgentTask, Registry, start_agent};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, Semaphore};

tokio::task_local! { static CHILD_WORKSPACE: (PathBuf, worktree::WorkspaceLease); }
/// Construction context consumed by WorkspaceRegistry before child tools exist.
pub(super) fn inherited_workspace() -> Option<(PathBuf, worktree::WorkspaceLease)> {
    CHILD_WORKSPACE.try_with(Clone::clone).ok()
}

const SCRIPT_LIMIT: usize = 512 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    script: Option<String>,
    name: Option<String>,
    script_path: Option<String>,
    resume_from_run_id: Option<String>,
    #[serde(default)]
    args: Value,
    description: Option<String>,
    title: Option<String>,
}
struct State {
    status: &'static str,
    output: Value,
    completed: BTreeMap<String, Value>,
    seen: BTreeSet<u64>,
    uncertain: BTreeSet<String>,
    children: Vec<AgentId>,
    phases: Vec<String>,
}
struct Run {
    session: String,
    workspace: PathBuf,
    cached: BTreeMap<String, Value>,
    lease: Mutex<Option<worktree::WorkspaceLease>>,
    path: PathBuf,
    meta: Value,
    state: Mutex<State>,
    done: Notify,
    cancelled: AtomicBool,
    admission: tokio::sync::Mutex<()>,
    permits: Semaphore,
    runtime: Mutex<Option<Arc<ToolRuntime>>>,
    cleaned: AtomicBool,
}
pub(super) struct Workflow {
    workspace: Arc<worktree::Workspace>,
    parent: AgentHandle,
    registry: Arc<Registry>,
    runs: Mutex<BTreeMap<String, Arc<Run>>>,
}
impl Workflow {
    pub(super) fn new(
        workspace: Arc<worktree::Workspace>,
        parent: AgentHandle,
        registry: Arc<Registry>,
    ) -> Self {
        Self {
            workspace,
            parent,
            registry,
            runs: Mutex::new(BTreeMap::new()),
        }
    }
    fn lookup(&self, session: &str, id: &str) -> Result<Arc<Run>, String> {
        self.runs
            .lock()
            .map_err(|_| "workflow state poisoned")?
            .get(id)
            .filter(|run| run.session == session)
            .cloned()
            .ok_or_else(|| "unknown workflow in this session".into())
    }
    async fn source(&self, workspace: &std::path::Path, input: &Input) -> Result<String, String> {
        let path = if let Some(path) = &input.script_path {
            Some(workspace.join(path))
        } else if input.script.is_some() {
            None
        } else if let Some(name) = &input.name {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err("invalid saved workflow name".into());
            }
            Some(
                workspace
                    .join(".claude/workflows")
                    .join(format!("{name}.js")),
            )
        } else {
            return Err("provide script, name, or scriptPath".into());
        };
        let source = if let Some(path) = path {
            let root = tokio::fs::canonicalize(workspace)
                .await
                .map_err(|e| e.to_string())?;
            let path = tokio::fs::canonicalize(path)
                .await
                .map_err(|e| e.to_string())?;
            if !path.starts_with(root) {
                return Err("workflow script must be inside the workspace".into());
            }
            if tokio::fs::metadata(&path)
                .await
                .map_err(|e| e.to_string())?
                .len()
                > SCRIPT_LIMIT as u64
            {
                return Err("workflow script exceeds 512 KiB".into());
            }
            use tokio::io::AsyncReadExt;
            let mut source = String::new();
            tokio::fs::File::open(path)
                .await
                .map_err(|e| e.to_string())?
                .take(SCRIPT_LIMIT as u64 + 1)
                .read_to_string(&mut source)
                .await
                .map_err(|e| e.to_string())?;
            source
        } else {
            input.script.clone().unwrap_or_default()
        };
        if source.len() > SCRIPT_LIMIT
            || serde_json::to_vec(&input.args)
                .map_err(|e| e.to_string())?
                .len()
                > OUTPUT_LIMIT
        {
            return Err("workflow input exceeds its size limit".into());
        }
        Ok(source)
    }
    async fn start(
        self: &Arc<Self>,
        input: Value,
        session: &str,
    ) -> Result<ClaudeToolReply, String> {
        if !self.registry.is_root_session(session).await {
            return Err("Workflow is only available to the opted-in root session".into());
        }
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let _ = (&input.description, &input.title);
        if input.script_path.is_none() && input.script.is_some() && input.name.is_some() {
            return Err("script and name are mutually exclusive".into());
        }
        let (workspace, lease) = self.workspace.pin_current();
        let source = self.source(&workspace, &input).await?;
        let (meta, body) = parse_meta(&source)?;
        let (completed, uncertain) = if let Some(prior) = &input.resume_from_run_id {
            let prior = self.lookup(session, prior)?;
            if prior.workspace != workspace {
                return Err("resume requires the original workflow workspace; it cannot retarget cached calls".into());
            }
            let state = prior.state.lock().map_err(|_| "workflow state poisoned")?;
            if state.status == "running" {
                return Err("stop the prior workflow before resuming".into());
            }
            (state.completed.clone(), state.uncertain.clone())
        } else {
            (BTreeMap::new(), BTreeSet::new())
        };
        let id = format!("wf_{}", uuid::Uuid::new_v4());
        let root = tokio::fs::canonicalize(&workspace)
            .await
            .map_err(|e| e.to_string())?;
        // An unpredictable private directory avoids following workspace-authored symlinks.
        let dir = root.join(format!(".nanocodex-workflow-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&dir)
            .await
            .map_err(|e| e.to_string())?;
        let path = dir.join(format!("{id}.js"));
        tokio::fs::write(&path, &source)
            .await
            .map_err(|e| e.to_string())?;
        let run = Arc::new(Run {
            session: session.into(),
            workspace: workspace.clone(),
            cached: completed.clone(),
            lease: Mutex::new(Some(lease)),
            path: path.clone(),
            meta: meta.clone(),
            state: Mutex::new(State {
                status: "running",
                output: Value::Null,
                completed,
                seen: BTreeSet::new(),
                uncertain,
                children: vec![],
                phases: vec![],
            }),
            done: Notify::new(),
            cancelled: AtomicBool::new(false),
            admission: tokio::sync::Mutex::new(()),
            permits: Semaphore::new(4),
            runtime: Mutex::new(None),
            cleaned: AtomicBool::new(false),
        });
        let tools = Tools::builder()
            .without_defaults()
            .tool(Bridge {
                owner: self.clone(),
                run: run.clone(),
            })
            .build()
            .map_err(|e| e.to_string())?;
        let runtime = Arc::new(ToolRuntime::new_with_tools(root, None, None, &tools));
        *run.runtime.lock().map_err(|_| "workflow state poisoned")? = Some(runtime.clone());
        self.runs
            .lock()
            .map_err(|_| "workflow state poisoned")?
            .insert(id.clone(), run.clone());
        let code = format!(
            "{}\nconst args = {};\nconst meta = {};\nconst result = await (async () => {{\n{}\n}})();\nawait tools.workflow_bridge({{op:'finish', value: result === undefined ? null : result}});",
            include_str!("workflow_helpers.js"),
            input.args,
            meta,
            body
        );
        let owner = self.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(300), async {
                let context = || ToolContext::new("workflow", &run.session, &task_id, &[], 16000);
                let mut result = runtime
                    .execute_code(&code, context())
                    .await
                    .map_err(|e| e.to_string())?;
                while result.cell.as_ref().is_some_and(|cell| cell.running) {
                    // Each workflow owns a new runtime with exactly one cell.
                    let cell = "1";
                    result = runtime
                        .wait_for_code(
                            &json!({"cell_id":cell,"yield_time_ms":1000,"max_tokens":16000})
                                .to_string(),
                            context(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                }
                if !result.success {
                    return Err(runtime_reply(&result.output, false)
                        .map(|reply| format!("{:?}", reply.content))
                        .unwrap_or_else(|e| e));
                }
                Ok::<(), String>(())
            })
            .await;
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some("workflow exceeded five-minute deadline".into()),
            };
            let stopped = run.cancelled.load(Ordering::SeqCst);
            let cleanup = owner.cancel_children(&run).await;
            runtime.control().cancel().await;
            {
                let mut state = run.state.lock().unwrap();
                if let Err(cleanup) = &cleanup {
                    state.status = "cleanup_failed";
                    state.output = json!({"error":bounded(cleanup)});
                } else if stopped {
                    state.status = "stopped";
                } else if let Some(error) = error {
                    state.status = "failed";
                    state.output = json!({"error":bounded(&error)});
                } else {
                    state.status = "completed";
                }
            }
            // Break the private runtime -> bridge -> run ownership cycle.
            run.runtime.lock().unwrap().take();
            if cleanup.is_ok() {
                run.lease.lock().unwrap().take();
            }
            run.done.notify_waiters();
        });
        Ok(text_reply(
            json!({"task_id":id,"status":"running","scriptPath":path,"meta":meta}).to_string(),
        ))
    }
    async fn cancel_children(&self, run: &Run) -> Result<(), String> {
        run.cancelled.store(true, Ordering::SeqCst);
        let _admission = run.admission.lock().await;
        let children = run.state.lock().unwrap().children.clone();
        let results = futures_util::future::join_all(children.into_iter().map(|id| async move {
            let (summaries, _) = self
                .registry
                .wait(&run.session, &[id], Duration::ZERO)
                .await
                .map_err(|e| e.to_string())?;
            if summaries.iter().any(|summary| summary.status.is_active()) {
                self.registry
                    .interrupt(&run.session, id)
                    .await
                    .map_err(|e| format!("workflow child {id} cancellation uncertain: {e}"))?;
            }
            Ok::<(), String>(())
        }))
        .await;
        for result in results {
            result?;
        }
        run.cleaned.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub(super) async fn output(
        &self,
        session: &str,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> Result<ClaudeToolReply, String> {
        let run = self.lookup(session, id)?;
        let notified = run.done.notified();
        if block && run.state.lock().unwrap().status == "running" {
            let _ =
                tokio::time::timeout(Duration::from_millis(timeout.min(300000)), notified).await;
        }
        let state = run.state.lock().unwrap();
        let receipt = json!({"task_id":id,"status":state.status,"output":state.output,"scriptPath":run.path,"meta":run.meta,"phases":state.phases,"agents":state.children}).to_string();
        if receipt.len() > OUTPUT_LIMIT {
            return Err("workflow receipt exceeds 64 KiB; reduce the returned result".into());
        }
        Ok(text_reply(receipt))
    }
    pub(super) async fn stop(&self, session: &str, id: &str) -> Result<ClaudeToolReply, String> {
        let run = self.lookup(session, id)?;
        if !run.cleaned.load(Ordering::SeqCst) {
            self.cancel_children(&run).await?;
            let runtime = run.runtime.lock().unwrap().clone();
            if let Some(runtime) = runtime {
                runtime.control().cancel().await;
            }
            run.state.lock().unwrap().status = "stopped";
            run.lease.lock().unwrap().take();
            run.done.notify_waiters();
        }
        self.output(session, id, false, 0).await
    }
}
fn bounded(text: &str) -> String {
    text.chars().take(OUTPUT_LIMIT / 4).collect()
}
struct Bridge {
    owner: Arc<Workflow>,
    run: Arc<Run>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    label: Option<String>,
    phase: Option<String>,
    schema: Option<Value>,
}
#[async_trait]
impl Tool for Bridge {
    fn definition(&self) -> HostDefinition {
        HostDefinition::function(
            "workflow_bridge",
            "Private workflow host bridge",
            json!({"type":"object","additionalProperties":true}),
        )
    }
    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let value: Value = input.decode_json()?;
        let result = self.dispatch(value).await.map_err(std::io::Error::other)?;
        Ok(ToolOutput::from_json(result, true))
    }
}
impl Bridge {
    async fn dispatch(&self, v: Value) -> Result<Value, String> {
        if self.run.cancelled.load(Ordering::SeqCst) {
            return Err("workflow stopped".into());
        }
        match v["op"].as_str() {
            Some("finish") => {
                let value = v.get("value").cloned().unwrap_or(Value::Null);
                if value.to_string().len() > OUTPUT_LIMIT {
                    return Err("workflow output exceeds 64 KiB".into());
                }
                self.run.state.lock().unwrap().output = value;
                Ok(Value::Null)
            }
            Some("phase") => {
                let title = v["title"].as_str().ok_or("phase requires a title")?;
                self.check_phase(title)?;
                let mut state = self.run.state.lock().unwrap();
                if state.phases.len() >= 64 {
                    return Err("phase limit exceeded".into());
                }
                state.phases.push(title.into());
                Ok(Value::Null)
            }
            Some("agent") => self.agent(v).await,
            _ => Err("unknown workflow operation".into()),
        }
    }
    fn check_phase(&self, title: &str) -> Result<(), String> {
        if self.run.meta["phases"]
            .as_array()
            .is_some_and(|phases| phases.iter().any(|p| p["title"] == title))
        {
            Ok(())
        } else {
            Err("phase title must match meta.phases exactly".into())
        }
    }
    async fn agent(&self, v: Value) -> Result<Value, String> {
        let ordinal = v["ordinal"]
            .as_u64()
            .filter(|n| *n < 15)
            .ok_or("workflow permits at most 15 agent calls")?;
        let prompt = v["prompt"]
            .as_str()
            .filter(|p| !p.trim().is_empty() && p.len() <= SCRIPT_LIMIT)
            .ok_or("agent requires a bounded nonblank prompt")?;
        let opts: Options = serde_json::from_value(v["opts"].clone()).map_err(|e| e.to_string())?;
        if let Some(phase) = &opts.phase {
            self.check_phase(phase)?;
        }
        let key = json!([prompt, v["opts"]]).to_string();
        {
            let mut state = self.run.state.lock().unwrap();
            if !state.seen.insert(ordinal) {
                return Err("duplicate workflow call ordinal".into());
            }
            if state.uncertain.contains(&key) {
                return Err("matching prior agent call has no confirmed successful result; refusing to repeat".into());
            }
            if let Some(cached) = self.run.cached.get(&key) {
                return Ok(cached.clone());
            }
            state.uncertain.insert(key.clone());
        }
        let _permit = self
            .run
            .permits
            .acquire()
            .await
            .map_err(|e| e.to_string())?;
        let id = {
            let _admission = self.run.admission.lock().await;
            if self.run.cancelled.load(Ordering::SeqCst) {
                return Err("workflow stopped".into());
            }
            let pinned = (
                self.run.workspace.clone(),
                self.run
                    .lease
                    .lock()
                    .unwrap()
                    .as_ref()
                    .ok_or("workflow workspace lease has ended")?
                    .clone(),
            );
            let admission = agents::profiles::Admission::default();
            let report = agents::profiles::scope(
                admission.clone(),
                CHILD_WORKSPACE.scope(
                    pinned,
                    start_agent(
                        &self.owner.parent,
                        &self.owner.registry,
                        &self.run.session,
                        AgentTask {
                            role: opts.label.unwrap_or_else(|| format!("workflow-{ordinal}")),
                            task: format!(
                                "{prompt}\n\nUse SubmitResult to submit the requested result."
                            ),
                            output_schema: opts.schema.unwrap_or_else(|| json!({"type":"string"})),
                        },
                    ),
                ),
            )
            .await
            .map_err(|e| e.to_string())?;
            self.run
                .state
                .lock()
                .unwrap()
                .children
                .push(report.agent_id);
            let id = report
                .agent_id
                .to_string()
                .parse::<u64>()
                .map_err(|e| e.to_string())?;
            agents::profiles::attach(&admission, id)?;
            report.agent_id
        };
        loop {
            let (summaries, _) = self
                .owner
                .registry
                .wait(&self.run.session, &[id], Duration::from_secs(1))
                .await
                .map_err(|e| e.to_string())?;
            let status = &summaries.first().ok_or("missing workflow child")?.status;
            match status {
                AgentStatus::Completed { output } => {
                    if output.to_string().len() > OUTPUT_LIMIT {
                        return Err("agent output exceeds 64 KiB".into());
                    }
                    let mut state = self.run.state.lock().unwrap();
                    state.completed.insert(key.clone(), output.clone());
                    state.uncertain.remove(&key);
                    return Ok(output.clone());
                }
                AgentStatus::Pending | AgentStatus::Running | AgentStatus::Closing => {}
                _ => {
                    return Err(format!(
                        "workflow agent {id} did not complete successfully: {status:?}"
                    ));
                }
            }
            if self.run.cancelled.load(Ordering::SeqCst) {
                return Err("workflow stopped".into());
            }
        }
    }
}
pub(super) fn install(native: ClaudeTools, workflow: Arc<Workflow>) -> ClaudeTools {
    let definition=serde_json::from_value(json!({"name":"Workflow","description":"Execute an explicitly authorized workflow in a private JavaScript sandbox. Requires --claude-workflows at root startup. Starts in the background; use TaskOutput/TaskStop with wf_ IDs. Script must begin export const meta = {name, description, phases?}, a pure literal. Helpers: agent(prompt,{label?,phase?,schema?}), parallel([()=>...]), pipeline(items,...stages), phase(title, optionalFunction). Only registry agents are available; no filesystem, network or process bridge. Hard budget: 15 agent calls, 4 concurrent, 5 minutes, 512 KiB script and 64 KiB result. Resume accepts same-session terminal runs and reuses only confirmed completed matching calls; uncertain calls are fenced.","input_schema":{"type":"object","properties":{"script":{"type":"string","maxLength":524288},"name":{"type":"string"},"description":{"type":"string"},"title":{"type":"string"},"args":{},"scriptPath":{"type":"string"},"resumeFromRunId":{"type":"string","pattern":"^wf_[a-z0-9-]{6,}$"}},"additionalProperties":false}})).expect("workflow definition");
    native.tool_with_context(definition, move |input, context| {
        let workflow = workflow.clone();
        async move { workflow.start(input, &context.session_id).await }
    })
}

// Parse only literal values before starting any runtime or child. This is not eval.
fn parse_meta(source: &str) -> Result<(Value, &str), String> {
    let rest = source
        .trim_start()
        .strip_prefix("export const meta")
        .ok_or("workflow must begin with export const meta = {...}")?
        .trim_start()
        .strip_prefix('=')
        .ok_or("meta requires a literal initializer")?;
    let mut parser = Literal {
        source: rest,
        pos: 0,
    };
    let meta = parser.value(0)?;
    parser.space();
    if parser.source[parser.pos..].starts_with(';') {
        parser.pos += 1;
    }
    let object = meta.as_object().ok_or("meta must be an object literal")?;
    if meta.to_string().len() > 8192 {
        return Err("workflow metadata exceeds 8 KiB".into());
    }
    for field in ["name", "description"] {
        if !object
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 1024 && !s.contains('\n'))
        {
            return Err(format!(
                "meta.{field} must be a nonblank single-line string"
            ));
        }
    }
    if let Some(phases) = object.get("phases") {
        let phases = phases
            .as_array()
            .filter(|p| p.len() <= 64)
            .ok_or("meta.phases must be a bounded array")?;
        for phase in phases {
            if !phase["title"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 1024)
            {
                return Err("meta phase requires a title".into());
            }
        }
    }
    Ok((meta, &rest[parser.pos..]))
}
struct Literal<'a> {
    source: &'a str,
    pos: usize,
}
impl Literal<'_> {
    fn space(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    fn take(&mut self, b: u8) -> bool {
        self.space();
        if self.source.as_bytes().get(self.pos) == Some(&b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.space();
        let quote = *self
            .source
            .as_bytes()
            .get(self.pos)
            .ok_or("missing literal string")?;
        if quote != b'\'' && quote != b'"' {
            return Err("expected literal string".into());
        }
        self.pos += 1;
        let mut result = String::new();
        loop {
            let ch = self.source[self.pos..]
                .chars()
                .next()
                .ok_or("unterminated literal string")?;
            self.pos += ch.len_utf8();
            if ch as u32 == quote as u32 {
                return Ok(result);
            }
            if ch == '\\' {
                let escaped = self.source[self.pos..]
                    .chars()
                    .next()
                    .ok_or("unterminated escape")?;
                self.pos += escaped.len_utf8();
                result.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    _ => return Err("unsupported literal escape".into()),
                });
            } else if ch == '\n' || ch == '\r' {
                return Err("newline in literal string".into());
            } else {
                result.push(ch);
            }
        }
    }
    fn key(&mut self) -> Result<String, String> {
        self.space();
        if matches!(self.source.as_bytes().get(self.pos), Some(b'\'' | b'"')) {
            return self.string();
        }
        let start = self.pos;
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("meta requires literal property names".into());
        }
        Ok(self.source[start..self.pos].into())
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 16 {
            return Err("meta exceeds literal nesting limit".into());
        }
        self.space();
        if self.take(b'{') {
            let mut map = serde_json::Map::new();
            if self.take(b'}') {
                return Ok(Value::Object(map));
            }
            loop {
                let key = self.key()?;
                if !self.take(b':') {
                    return Err("meta requires literal properties".into());
                }
                let value = self.value(depth + 1)?;
                if map.insert(key, value).is_some() {
                    return Err("duplicate meta property".into());
                }
                if self.take(b'}') {
                    break;
                }
                if !self.take(b',') {
                    return Err("meta must be pure literal".into());
                }
                if self.take(b'}') {
                    break;
                }
            }
            return Ok(Value::Object(map));
        }
        if self.take(b'[') {
            let mut values = vec![];
            if self.take(b']') {
                return Ok(json!(values));
            }
            loop {
                values.push(self.value(depth + 1)?);
                if self.take(b']') {
                    break;
                }
                if !self.take(b',') {
                    return Err("meta must be pure literal".into());
                }
                if self.take(b']') {
                    break;
                }
            }
            return Ok(json!(values));
        }
        if matches!(self.source.as_bytes().get(self.pos), Some(b'\'' | b'"')) {
            return Ok(json!(self.string()?));
        }
        let start = self.pos;
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.'))
        {
            self.pos += 1;
        }
        serde_json::from_str(&self.source[start..self.pos])
            .map_err(|_| "meta must contain only pure literals".into())
    }
}
