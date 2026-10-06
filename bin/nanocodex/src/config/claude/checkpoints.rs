//! Private, bounded before-images for native file tools. The final pre-hook
//! captures approved input; the first post-hook records the actual filesystem.
//! Rewind changes files only, never conversation history or arbitrary effects.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fs2::FileExt;
use nanocodex::claude::{
    ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation, ClaudeToolReply,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_JOURNAL: u64 = 32 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
struct Image {
    data: String,
    mode: u32,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    turn: String,
    call: String,
    tool: String,
    path: PathBuf,
    #[serde(default)]
    workspace: Option<PathBuf>,
    before: Option<Image>,
    after: Option<String>,
    complete: bool,
    restored: bool,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    session: String,
    workspace: PathBuf,
    entries: Vec<Entry>,
    #[serde(default)]
    restore_interrupted: bool,
}

type WorkspaceResolver = Arc<dyn Fn(&str) -> Result<PathBuf> + Send + Sync>;

pub(crate) struct Checkpoints {
    current_workspace: WorkspaceResolver,
    directory: PathBuf,
}
impl Checkpoints {
    pub(crate) fn new_with_workspace(current_workspace: WorkspaceResolver, home: PathBuf) -> Self {
        Self {
            current_workspace,
            directory: home.join("claude/checkpoints"),
        }
    }
    fn before_file(
        &self,
        name: &str,
        input: &Value,
        invocation: &ClaudeToolInvocation,
    ) -> Result<()> {
        let Some(key) = path_key(name) else {
            return Ok(());
        };
        let raw = input[key]
            .as_str()
            .ok_or("checkpoint: file path is required")?;
        let workspace = (self.current_workspace)(&invocation.session_id)?;
        let path = relative(&workspace, Path::new(raw))?;
        let image = read_image(&workspace, &path)?;
        let (_lock, filename) = lock(&self.directory, &invocation.session_id, true)?;
        let mut journal = load(&filename)?.unwrap_or_else(|| Journal {
            version: 1,
            session: invocation.session_id.clone(),
            workspace: workspace.clone(),
            entries: Vec::new(),
            restore_interrupted: false,
        });
        validate(&journal, &invocation.session_id)?;
        if journal.restore_interrupted {
            return Err(
                "checkpoint restore was interrupted; inspect files and journal before editing"
                    .into(),
            );
        }
        if journal
            .entries
            .iter()
            .any(|e| e.turn == invocation.turn_id && e.call == invocation.call_id)
        {
            return Err(
                "checkpoint invocation already recorded; refusing uncertain repeated file effect"
                    .into(),
            );
        }
        if journal.entries.len() >= MAX_ENTRIES {
            return Err("checkpoint entry limit reached (512); start a new session".into());
        }
        journal.entries.push(Entry {
            turn: invocation.turn_id.clone(),
            call: invocation.call_id.clone(),
            tool: name.into(),
            path,
            workspace: Some(workspace),
            before: image,
            after: None,
            complete: false,
            restored: false,
        });
        save(&filename, &journal)
    }
    fn after_file(&self, name: &str, invocation: &ClaudeToolInvocation) -> Result<()> {
        if path_key(name).is_none() {
            return Ok(());
        }
        let (_lock, filename) = lock(&self.directory, &invocation.session_id, false)?;
        let mut journal = load(&filename)?.ok_or("checkpoint before-image missing")?;
        let entry = journal
            .entries
            .iter_mut()
            .find(|e| e.turn == invocation.turn_id && e.call == invocation.call_id)
            .ok_or("checkpoint invocation missing")?;
        let workspace = entry.workspace.as_ref().unwrap_or(&journal.workspace);
        entry.after = fingerprint(&read_image(workspace, &entry.path)?);
        entry.complete = true;
        save(&filename, &journal)
    }
}
impl ClaudeToolHooks for Checkpoints {
    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision>> {
        Box::pin(async move {
            self.before_file(name, input, invocation)
                .map(|()| ClaudeToolDecision::Allow)
        })
    }
    fn after<'a>(
        &'a self,
        name: &'a str,
        _input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
        _reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<()>> {
        Box::pin(async move { self.after_file(name, invocation) })
    }
}
fn path_key(name: &str) -> Option<&'static str> {
    match name {
        "Edit" | "Write" => Some("file_path"),
        "NotebookEdit" => Some("notebook_path"),
        _ => None,
    }
}
fn relative(workspace: &Path, path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.strip_prefix(workspace)
            .map_err(|_| "checkpoint path outside workspace")?
    } else {
        path
    };
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("checkpoint path contains traversal or special components".into());
    }
    Ok(path.into())
}
fn checked_path(workspace: &Path, path: &Path) -> Result<PathBuf> {
    if fs::canonicalize(workspace).map_err(|e| e.to_string())? != workspace {
        return Err("checkpoint workspace moved or became a symlink".into());
    }
    let path = relative(workspace, path)?;
    let mut current = workspace.to_path_buf();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("checkpoint refuses symbolic links".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(current)
}
fn read_image(workspace: &Path, path: &Path) -> Result<Option<Image>> {
    let target = checked_path(workspace, path)?;
    let metadata = match fs::symlink_metadata(&target) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Err("checkpoint requires a regular file of at most 8 MiB".into());
    }
    let file = open_regular(&target, MAX_FILE)?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_FILE {
        return Err("checkpoint file grew beyond 8 MiB".into());
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = u32::from(metadata.permissions().readonly());
    Ok(Some(Image {
        data: STANDARD.encode(data),
        mode,
    }))
}
// Opening a concurrently replaced FIFO must not block a CLI or tool loop.
fn open_regular(path: &Path, limit: u64) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("checkpoint requires a bounded regular file".into());
    }
    Ok(file)
}
fn fingerprint(image: &Option<Image>) -> Option<String> {
    image.as_ref().map(|i| {
        let mut hash = Sha256::new();
        hash.update(i.data.as_bytes());
        hash.update(i.mode.to_le_bytes());
        hex::encode(hash.finalize())
    })
}
fn private_dir(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    if fs::symlink_metadata(directory)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("checkpoint directory must not be a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn lock(directory: &Path, session: &str, create: bool) -> Result<(File, PathBuf)> {
    if session.is_empty() || session.len() > 256 {
        return Err("checkpoint session ID must be 1..256 bytes".into());
    }
    if create {
        private_dir(directory)?;
    }
    if !directory.is_dir() {
        return Err("no native file checkpoints found".into());
    }
    let path = directory.join(format!(
        "{}.json",
        hex::encode(Sha256::digest(session.as_bytes()))
    ));
    if !create && !path.is_file() {
        return Err("no native file checkpoints found for this session".into());
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(directory.join(".lock"))
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("checkpoint lock must be a regular file".into());
    }
    file.try_lock_exclusive()
        .map_err(|_| "checkpoint store is busy; retry after the active file operation finishes")?;
    Ok((file, path))
}
fn load(filename: &Path) -> Result<Option<Journal>> {
    let metadata = match fs::symlink_metadata(filename) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.len() > MAX_JOURNAL {
        return Err("checkpoint journal is not a bounded regular file".into());
    }
    serde_json::from_reader(open_regular(filename, MAX_JOURNAL)?.take(MAX_JOURNAL + 1))
        .map(Some)
        .map_err(|e| format!("invalid checkpoint journal: {e}"))
}
fn validate(journal: &Journal, session: &str) -> Result<()> {
    if journal.version != 1 || journal.session != session || journal.entries.len() > MAX_ENTRIES {
        return Err("checkpoint journal version, identity or size mismatch".into());
    }
    Ok(())
}
fn save(filename: &Path, journal: &Journal) -> Result<()> {
    let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err(
            "checkpoint journal exceeds 32 MiB; file effect refused or final capture incomplete"
                .into(),
        );
    }
    let directory = filename.parent().ok_or("checkpoint directory missing")?;
    let mut total = bytes.len() as u64;
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path() != filename {
            total = total.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
        }
        if total > MAX_TOTAL {
            return Err(
                "checkpoint store exceeds 256 MiB; archive old sessions before editing".into(),
            );
        }
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(filename).map_err(|e| e.to_string())?;
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

/// A selected turn restores that turn and all later recorded native file calls.
/// Preview is read-only. Restoring requires the explicit CLI flag and exact turn.
pub(crate) fn rewind(
    home: &Path,
    session: &str,
    turn: Option<&str>,
    restore: bool,
) -> Result<Value> {
    if restore && turn.is_none() {
        return Err("--restore requires --checkpoint <turn-id>; preview the session first".into());
    }
    let (_lock, filename) = lock(&home.join("claude/checkpoints"), session, false)?;
    let mut journal = load(&filename)?.ok_or("checkpoint journal missing")?;
    validate(&journal, session)?;
    let mut turns: Vec<Value> = Vec::new();
    for entry in &journal.entries {
        if !turns.iter().any(|v| v["checkpoint"] == entry.turn) {
            turns.push(json!({"checkpoint":entry.turn,"files":journal.entries.iter().filter(|e| e.turn == entry.turn).map(|e| json!({"path":e.path,"workspace":e.workspace.as_ref().unwrap_or(&journal.workspace),"tool":e.tool,"complete":e.complete,"restored":e.restored,"created":e.before.is_none()})).collect::<Vec<_>>()}));
        }
    }
    let mut result = json!({"session":session,"workspace":journal.workspace,"scope":"Native Edit, Write and NotebookEdit files only. Conversation, Bash and MCP effects are not rewound.","checkpoints":turns,"restored":false});
    let Some(turn) = turn else {
        return Ok(result);
    };
    if journal.restore_interrupted {
        return Err("previous restore was interrupted; inspect the private journal and files before recovery".into());
    }
    if journal.entries.iter().any(|e| !e.complete) {
        return Err(
            "file operation is pending or its final capture failed; rewind is unavailable".into(),
        );
    }
    let start = journal
        .entries
        .iter()
        .position(|e| e.turn == turn)
        .ok_or("unknown checkpoint; use a turn ID from preview")?;
    let selected: Vec<_> = (start..journal.entries.len())
        .filter(|&i| !journal.entries[i].restored)
        .collect();
    if selected.is_empty() {
        return Err("checkpoint has already been restored".into());
    }
    let mut expected: BTreeMap<(PathBuf, PathBuf), Option<String>> = BTreeMap::new();
    let mut desired: BTreeMap<(PathBuf, PathBuf), Option<Image>> = BTreeMap::new();
    for &index in selected.iter().rev() {
        let entry = &journal.entries[index];
        let workspace = entry.workspace.as_ref().unwrap_or(&journal.workspace);
        let key = (workspace.clone(), entry.path.clone());
        let current = if let Some(image) = desired.get(&key) {
            fingerprint(image)
        } else {
            let value = fingerprint(&read_image(workspace, &entry.path)?);
            expected.insert(key.clone(), value.clone());
            value
        };
        if current != entry.after {
            return Err(format!(
                "external modification or checkpoint chain conflict: {}; nothing restored",
                entry.path.display()
            ));
        }
        desired.insert(key, entry.before.clone());
    }
    result["selected_checkpoint"] = json!(turn);
    result["changes"] = json!(desired.iter().map(|((workspace, path), image)| json!({"workspace":workspace,"path":path,"action":if image.is_some() {"restore"} else {"remove_created_file"}})).collect::<Vec<_>>());
    if !restore {
        return Ok(result);
    }
    // Persist the recovery fence before touching any file. A partial I/O failure
    // or process death can never be reported/retried as an atomic success.
    journal.restore_interrupted = true;
    save(&filename, &journal)?;
    for ((workspace, path), image) in desired {
        if fingerprint(&read_image(&workspace, &path)?)
            != expected[&(workspace.clone(), path.clone())]
        {
            return Err(format!(
                "file changed during restore: {}; restore interrupted; inspect files",
                path.display()
            ));
        }
        let target = checked_path(&workspace, &path)?;
        if let Some(image) = image {
            let bytes = STANDARD.decode(&image.data).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_FILE {
                return Err("checkpoint image exceeds limit; restore interrupted".into());
            }
            let parent = target.parent().ok_or("checkpoint file has no parent")?;
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            temporary.write_all(&bytes).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                temporary
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(image.mode))
                    .map_err(|e| e.to_string())?;
            }
            temporary.as_file().sync_all().map_err(|e| e.to_string())?;
            temporary.persist(&target).map_err(|e| e.to_string())?;
        } else if target.exists() {
            fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
        if let Some(parent) = target.parent() {
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    for index in selected {
        journal.entries[index].restored = true;
    }
    journal.restore_interrupted = false;
    save(&filename, &journal)?;
    result["restored"] = json!(true);
    Ok(result)
}
