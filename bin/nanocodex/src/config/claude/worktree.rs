//! Session-local workspaces. Git effects are fenced before admission and receipts
//! survive restart. No process cwd changes; consumers snapshot current() per call.
use super::*;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    process::Command,
    sync::RwLock,
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
struct Owned {
    original: PathBuf,
    path: PathBuf,
    branch: String,
    base: String,
    common: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: Value,
    output: Value,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    isolated_child: bool,
    #[serde(default)]
    profiles: Option<Vec<nanocodex::claude_tools::AgentProfile>>,
    session: String,
    current: PathBuf,
    active: Option<Owned>,
    generation: u64,
    pending: Option<String>,
    receipts: BTreeMap<String, Receipt>,
}

/// Prepare the fresh conversation's immutable workspace/profile snapshot before
/// publishing its journal. Git cleanup ownership stays with the source session.
pub(super) fn prepare_rewind_branch(home: &Path, source: &str, target: &str) -> Result<()> {
    if source == target || source.is_empty() || target.is_empty() {
        return Err("rewind requires distinct nonempty workspace session identities".into());
    }
    let directory = home.join("claude/workspaces");
    let source_path = directory.join(format!("{}.json", digest(source)));
    match fs::symlink_metadata(&source_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("rewind source workspace sidecar must be a regular file".into());
        }
        Ok(_) => {}
    }
    let target_path = directory.join(format!("{}.json", digest(target)));
    let mut locks = Vec::new();
    for path in [&source_path, &target_path] {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "another process is changing a rewind session workspace")?;
        locks.push(lock);
    }
    let mut bytes = Vec::new();
    fs::File::open(&source_path)
        .map_err(|error| error.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("workspace state exceeds size limit".into());
    }
    let mut state: State = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if state.version != 1 || state.session != source || state.pending.is_some() {
        return Err("rewind source workspace identity/version/transition is unavailable".into());
    }
    let current = fs::canonicalize(&state.current).map_err(|error| error.to_string())?;
    if current != state.current || !current.is_dir() {
        return Err("saved workspace identity changed".into());
    }
    if let Some(owned) = &state.active {
        validate_owned(owned)?;
        if owned.path != state.current {
            return Err("rewind source owned worktree does not match its workspace".into());
        }
    }
    state.session = target.into();
    state.active = None;
    state.isolated_child = false;
    state.generation = 0;
    state.receipts.clear();
    let mut file = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&state).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist_noclobber(&target_path)
        .map_err(|e| e.to_string())?;
    fs::File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) struct Workspace {
    state: RwLock<State>,
    path: PathBuf,
    files: std::sync::Mutex<BTreeMap<PathBuf, Arc<ClaudeWorkspaceFiles>>>,
    notebooks: std::sync::Mutex<BTreeMap<PathBuf, Arc<nanocodex::claude_tools::ClaudeNotebook>>>,
    pins: Arc<std::sync::Mutex<BTreeMap<PathBuf, usize>>>,
    // Conservative lifetime: retained until the child controller is dropped.
    parent_lease: std::sync::Mutex<Option<WorkspaceLease>>,
}
impl Workspace {
    pub(super) fn new(initial: PathBuf, home: PathBuf, session_id: &str) -> Result<Self> {
        let initial = fs::canonicalize(initial).map_err(|e| e.to_string())?;
        let directory = home.join("claude/workspaces");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join(format!("{}.json", digest(session_id)));
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "another process is changing this session workspace")?;
        let state = match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 2 * 1024 * 1024 {
                    return Err("workspace state exceeds size limit".into());
                }
                let state: State = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if state.version != 1 || state.session != session_id {
                    return Err("workspace state identity/version mismatch".into());
                }
                if let Some(pending) = &state.pending {
                    return Err(format!(
                        "worktree transition has uncertain outcome ({pending}); inspect {} before recovery; refusing effect replay",
                        path.display()
                    ));
                }
                let canonical = fs::canonicalize(&state.current)
                    .map_err(|e| format!("saved workspace is unavailable: {e}"))?;
                if canonical != state.current || !canonical.is_dir() {
                    return Err("saved workspace identity changed".into());
                }
                if let Some(owned) = &state.active {
                    validate_owned(owned)?;
                }
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let state = State {
                    version: 1,
                    isolated_child: false,
                    profiles: None,
                    session: session_id.into(),
                    current: initial,
                    active: None,
                    generation: 0,
                    pending: None,
                    receipts: BTreeMap::new(),
                };
                // Persist a child's initial pin even when it never transitions.
                // Reopening after the parent exits must not retarget that child.
                save(&path, &state)?;
                state
            }
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            state: RwLock::new(state),
            path,
            files: std::sync::Mutex::new(BTreeMap::new()),
            notebooks: std::sync::Mutex::new(BTreeMap::new()),
            pins: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            parent_lease: std::sync::Mutex::new(None),
        })
    }
    /// Snapshot and pin atomically against parent transitions. The registry must
    /// retain this controller for as long as the child can access its workspace.
    pub(super) fn child(&self, home: PathBuf, session_id: &str) -> Result<Self> {
        let (initial, lease) = self.pin_current();
        Self::child_from_pin(home, session_id, initial, lease)
    }
    /// The supplied lease preserves the exact admitted workspace even when its
    /// parent has since transitioned. The child keeps the pin until closure.
    pub(super) fn child_from_pin(
        home: PathBuf,
        session_id: &str,
        initial: PathBuf,
        lease: WorkspaceLease,
    ) -> Result<Self> {
        if lease.root != initial {
            return Err("workspace lease does not match child snapshot".into());
        }
        let mut child = Self::new(initial, home, session_id)?;
        child.parent_lease = std::sync::Mutex::new(Some(lease));
        Ok(child)
    }
    pub(super) fn profiles(&self) -> Option<Vec<nanocodex::claude_tools::AgentProfile>> {
        self.state
            .read()
            .expect("workspace state poisoned")
            .profiles
            .clone()
    }
    pub(super) fn bind_profiles(
        &self,
        profiles: Vec<nanocodex::claude_tools::AgentProfile>,
    ) -> Result<()> {
        let mut state = self.state.write().map_err(|_| "workspace state poisoned")?;
        let mut next = state.clone();
        if next.profiles.is_some() {
            return Err("child profile binding already exists".into());
        }
        next.profiles = Some(profiles);
        save(&self.path, &next)?;
        *state = next;
        Ok(())
    }
    pub(super) fn release_parent(&self) {
        if let Ok(mut lease) = self.parent_lease.lock() {
            lease.take();
        }
    }
    /// Isolate only this child controller. The parent remains pinned and unchanged.
    pub(super) fn isolate_child(&self, child: &str) -> Result<()> {
        self.execute(
            "EnterWorktree",
            json!({"name": format!("agent-{}", &digest(child)[..20])}),
            &format!("child-isolation:{child}"),
        )?;
        let mut state = self.state.write().map_err(|_| "workspace state poisoned")?;
        let mut next = state.clone();
        next.isolated_child = true;
        save(&self.path, &next)?;
        *state = next;
        Ok(())
    }
    pub(super) fn isolated_status(&self) -> Value {
        let state = self.state.read().expect("workspace state poisoned");
        match (&state.active, state.isolated_child) {
            (Some(owned), true) => {
                json!({"workspace":owned.path,"branch":owned.branch,"base":owned.base,"cleanup":"retained for resume; CloseAgent removes only an unchanged unpinned worktree"})
            }
            _ => Value::Null,
        }
    }
    /// Called only after the registry confirms closure of this runtime and all
    /// descendants. Dirty, committed, externally changed or pinned trees remain.
    pub(super) fn finish_child(&self, child: &str) -> Value {
        if let Ok(mut lease) = self.parent_lease.lock() {
            lease.take();
        }
        let before = self.isolated_status();
        if before.is_null() {
            return before;
        }
        match self.execute(
            "ExitWorktree",
            json!({"cleanup":true}),
            &format!("close-child:{child}"),
        ) {
            Ok(receipt) => receipt,
            Err(reason) => json!({"kept":true,"worktree":before,"reason":reason}),
        }
    }
    pub(super) fn saved_current(home: &Path, session_id: &str) -> Result<PathBuf> {
        let path = home
            .join("claude/workspaces")
            .join(format!("{}.json", digest(session_id)));
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("workspace state exceeds size limit".into());
        }
        let state: State = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if state.version != 1 || state.session != session_id || state.pending.is_some() {
            return Err("session workspace unavailable during an uncertain transition".into());
        }
        let current = fs::canonicalize(&state.current).map_err(|error| error.to_string())?;
        if current != state.current || !current.is_dir() {
            return Err("saved workspace identity changed".into());
        }
        Ok(current)
    }
    pub(super) fn current(&self) -> PathBuf {
        self.state
            .read()
            .expect("workspace state poisoned")
            .current
            .clone()
    }
    pub(super) fn pin_current(&self) -> (PathBuf, WorkspaceLease) {
        let state = self.state.read().expect("workspace state poisoned");
        let root = state.current.clone();
        *self
            .pins
            .lock()
            .expect("workspace pins poisoned")
            .entry(root.clone())
            .or_default() += 1;
        let lease = WorkspaceLease {
            root: root.clone(),
            pins: self.pins.clone(),
        };
        (root, lease)
    }
    pub(super) fn files(&self) -> Result<Arc<ClaudeWorkspaceFiles>> {
        let root = self.current();
        let mut files = self
            .files
            .lock()
            .map_err(|_| "workspace files lock poisoned")?;
        if let Some(files) = files.get(&root) {
            return Ok(files.clone());
        }
        let adapter = Arc::new(ClaudeWorkspaceFiles::new(&root)?);
        files.insert(root, adapter.clone());
        Ok(adapter)
    }
    pub(super) fn notebook(&self) -> Result<Arc<nanocodex::claude_tools::ClaudeNotebook>> {
        let root = self.current();
        let mut notebooks = self
            .notebooks
            .lock()
            .map_err(|_| "workspace notebook lock poisoned")?;
        if let Some(notebook) = notebooks.get(&root) {
            return Ok(notebook.clone());
        }
        let adapter = Arc::new(nanocodex::claude_tools::ClaudeNotebook::new(&root)?);
        notebooks.insert(root, adapter.clone());
        Ok(adapter)
    }
    fn execute(&self, name: &str, input: Value, key: &str) -> Result<Value> {
        let mut state = self.state.write().map_err(|_| "workspace state poisoned")?;
        if let Some(receipt) = state.receipts.get(key) {
            if receipt.input != json!({"name":name,"input":input}) {
                return Err("worktree invocation identity reused with different arguments".into());
            }
            return Ok(receipt.output.clone());
        }
        if state.pending.is_some() {
            return Err("worktree transition outcome is uncertain; refusing effect replay".into());
        }
        if state.receipts.len() >= 256 {
            return Err("worktree transition limit reached (256)".into());
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "another process is changing this session workspace")?;
        if let Ok(bytes) = fs::read(&self.path) {
            let stored: State = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if stored.generation != state.generation || stored.pending.is_some() {
                return Err(
                    "session workspace changed in another process; reopen before continuing".into(),
                );
            }
        }
        let mut next = state.clone();
        let output = match name {
            "EnterWorktree" => self.enter(&mut next, input.clone(), key)?,
            "ExitWorktree" => self.exit(&mut next, input.clone(), key)?,
            _ => return Err("unknown worktree tool".into()),
        };
        next.generation += 1;
        next.pending = None;
        next.receipts.insert(
            key.into(),
            Receipt {
                input: json!({"name":name,"input":input}),
                output: output.clone(),
            },
        );
        save(&self.path, &next)?;
        *state = next;
        Ok(output)
    }
    fn enter(&self, state: &mut State, input: Value, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            name: Option<String>,
            path: Option<String>,
            repo_path: Option<String>,
        }
        let args: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        if args.path.is_some() {
            return Err("external/nonowned worktree paths require actual user approval; this host does not support that approval flow (unsupported; no mutation)".into());
        }
        if state.active.is_some() {
            return Err("session is already in an owned worktree; ExitWorktree first".into());
        }
        let root = git_root(&state.current)?;
        if root != state.current {
            return Err(
                "EnterWorktree requires the exact Git repository root as session workspace".into(),
            );
        }
        if let Some(requested) = args.repo_path {
            let requested = PathBuf::from(requested);
            let requested = fs::canonicalize(if requested.is_absolute() {
                requested
            } else {
                root.join(requested)
            })
            .map_err(|e| format!("invalid repo_path: {e}"))?;
            if requested != root {
                return Err("repo_path must equal the exact current Git repository root".into());
            }
        }
        let name = args
            .name
            .unwrap_or_else(|| format!("session-{}", &digest(key)[..16]));
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(
                "worktree name must be 1-64 ASCII letters, digits, hyphens or underscores".into(),
            );
        }
        let base = git(&root, &["rev-parse", "--verify", "HEAD"])?;
        let common = common_dir(&root)?;
        let container = root.join(".claude/worktrees");
        // Reject symlink containers before creating or deleting anything.
        safe_container(&root)?;
        let path = container.join(&name);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(
                "worktree destination already exists; refusing to adopt or overwrite it".into(),
            );
        }
        let branch = format!("claude/{name}");
        if Command::new("git")
            .current_dir(&root)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .map_err(|e| e.to_string())?
            .success()
        {
            return Err("worktree branch already exists; refusing to adopt it".into());
        }
        state.pending = Some(format!("enter:{key}:{}", path.display()));
        save(&self.path, state)?;
        fs::create_dir_all(&container)
            .map_err(|e| format!("worktree admission fenced; directory creation failed: {e}"))?;
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                "--",
                path.to_str().ok_or("worktree path must be UTF-8")?,
                &base,
            ],
        )
        .map_err(|e| format!("worktree admission fenced; Git outcome requires inspection: {e}"))?;
        let owned = Owned {
            original: root,
            path: path.clone(),
            branch: branch.clone(),
            base,
            common,
        };
        validate_owned(&owned)?;
        state.current = path.clone();
        state.active = Some(owned);
        Ok(
            json!({"status":"entered","workspace":path,"branch":branch,"owned":true,"exit_default":"keep"}),
        )
    }
    fn exit(&self, state: &mut State, input: Value, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            #[serde(default)]
            cleanup: bool,
        }
        let args: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let owned = state
            .active
            .clone()
            .ok_or("session has no owned active worktree")?;
        validate_owned(&owned)?;
        if git_root(&owned.original)? != owned.original
            || common_dir(&owned.original)? != owned.common
        {
            return Err("original repository identity changed; refusing exit".into());
        }
        if args.cleanup {
            if self
                .pins
                .lock()
                .map_err(|_| "workspace pins poisoned")?
                .get(&owned.path)
                .copied()
                .unwrap_or(0)
                > 0
            {
                return Err("worktree has active background or child contexts; stop them or KEEP the worktree".into());
            }
            let dirty = git(
                &owned.path,
                &[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored",
                ],
            )?;
            if !dirty.is_empty() {
                return Err("worktree has dirty, untracked, or ignored files; cleanup refused; use ExitWorktree without cleanup to preserve it".into());
            }
            if git(&owned.path, &["rev-parse", "HEAD"])? != owned.base {
                return Err(
                    "worktree contains new commits; cleanup refused to preserve unpublished work"
                        .into(),
                );
            }
            state.pending = Some(format!("cleanup:{key}:{}", owned.path.display()));
            save(&self.path, state)?;
            git(
                &owned.original,
                &[
                    "worktree",
                    "remove",
                    "--",
                    owned.path.to_str().ok_or("worktree path must be UTF-8")?,
                ],
            )
            .map_err(|e| format!("cleanup admission fenced; inspect worktree outcome: {e}"))?;
            git(&owned.original, &["branch", "-d", "--", &owned.branch])
                .map_err(|e| format!("cleanup admission fenced; worktree removed but branch cleanup requires inspection: {e}"))?;
        }
        state.current = owned.original.clone();
        state.active = None;
        Ok(
            json!({"status":"exited","workspace":owned.original,"worktree":owned.path,"branch":owned.branch,"kept":!args.cleanup}),
        )
    }
}

pub(super) struct WorkspaceLease {
    root: PathBuf,
    pins: Arc<std::sync::Mutex<BTreeMap<PathBuf, usize>>>,
}
impl Clone for WorkspaceLease {
    fn clone(&self) -> Self {
        *self
            .pins
            .lock()
            .expect("workspace pins poisoned")
            .entry(self.root.clone())
            .or_default() += 1;
        Self {
            root: self.root.clone(),
            pins: self.pins.clone(),
        }
    }
}
impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        if let Ok(mut pins) = self.pins.lock()
            && let Some(count) = pins.get_mut(&self.root)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pins.remove(&self.root);
            }
        }
    }
}

pub(super) fn install(mut tools: ClaudeTools, workspace: Arc<Workspace>) -> ClaudeTools {
    let definitions = [
        json!({"name":"EnterWorktree","description":"Create and enter a session-owned Git worktree under .claude/worktrees. Atomically switches this session's workspace; existing children/background jobs remain pinned. Requires exact repository root. Existing/external paths are unsupported without a host approval flow. Never adopts or overwrites an existing worktree.","input_schema":{"type":"object","properties":{"name":{"type":"string"},"repo_path":{"type":"string"},"path":{"type":"string"}},"additionalProperties":false}}),
        json!({"name":"ExitWorktree","description":"Return this session to its original repository. By default KEEP the owned worktree and branch, including dirty files. Explicit cleanup=true removes only this session's exact owned clean worktree and unchanged branch; dirty/ignored files or new commits refuse cleanup. Existing children/background jobs remain pinned.","input_schema":{"type":"object","properties":{"cleanup":{"type":"boolean","default":false}},"additionalProperties":false}}),
    ];
    for definition in definitions {
        let definition: ToolDefinition =
            serde_json::from_value(definition).expect("worktree definition");
        let name = definition.name.clone();
        let workspace = workspace.clone();
        tools = tools.tool_with_context(definition, move |input, invocation| {
            let workspace = workspace.clone();
            let name = name.clone();
            async move {
                let key = format!("{}:{}", invocation.turn_id, invocation.call_id);
                tokio::task::spawn_blocking(move || workspace.execute(&name, input, &key))
                    .await
                    .map_err(|e| e.to_string())?
                    .map(|value| text_reply(value.to_string()))
            }
        });
    }
    tools
}
fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn save(path: &Path, state: &State) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("missing state parent")?)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(state).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    fs::File::open(path.parent().ok_or("missing state parent")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim_end_matches(['\r', '\n']).into())
        .map_err(|e| e.to_string())
}
fn git_root(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(git(path, &["rev-parse", "--show-toplevel"])?).map_err(|e| e.to_string())
}
fn common_dir(path: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(git(path, &["rev-parse", "--git-common-dir"])?);
    fs::canonicalize(if common.is_absolute() {
        common
    } else {
        path.join(common)
    })
    .map_err(|e| e.to_string())
}
fn safe_container(root: &Path) -> Result<()> {
    for path in [root.join(".claude"), root.join(".claude/worktrees")] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(
                    "worktree container must be an ordinary directory inside the repository".into(),
                );
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
fn validate_owned(owned: &Owned) -> Result<()> {
    safe_container(&owned.original)?;
    let root = git_root(&owned.path)?;
    if root != owned.path
        || root.parent() != Some(owned.original.join(".claude/worktrees").as_path())
        || common_dir(&root)? != owned.common
    {
        return Err("owned worktree repository/path identity changed".into());
    }
    if git(&root, &["symbolic-ref", "--short", "HEAD"])? != owned.branch {
        return Err("owned worktree branch changed; refusing transition".into());
    }
    Ok(())
}
