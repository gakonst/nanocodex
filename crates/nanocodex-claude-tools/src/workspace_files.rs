//! Explicitly rooted, bounded workspace operations for a host-authorized Claude-style tool surface.
//!
//! Construct this module only after the host authorizes and isolates the workspace. The
//! path checks are defense in depth, not a substitute for OS-level isolation or permissions.

use crate::{ToolContent, ToolOutput, media::MediaReadOptions};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::portable_files::{MAX_FILE, MAX_VISITS};
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// A host-authorized directory used by workspace tools.
///
/// The host must explicitly authorize and OS-isolate this root. In particular, path
/// validation cannot eliminate races with a hostile process concurrently swapping
/// directory entries; do not use this as a security boundary against such processes.
#[derive(Clone, Debug)]
pub struct ClaudeWorkspaceFiles {
    root: PathBuf,
    root_alias: PathBuf,
    media: MediaReadOptions,
}

impl ClaudeWorkspaceFiles {
    /// Canonicalize an existing directory; no ambient/current workspace is assumed.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let given = root.as_ref();
        let root_alias = if given.is_absolute() {
            given.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("workspace current directory: {e}"))?
                .join(given)
        };
        let root = fs::canonicalize(given).map_err(|e| format!("workspace root: {e}"))?;
        if !root.is_dir() {
            return Err("workspace root is not a directory".into());
        }
        Ok(Self {
            root,
            root_alias,
            media: MediaReadOptions::default(),
        })
    }

    /// Configure trusted PDF helper executables. These paths are host configuration,
    /// never tool-call arguments. The host must isolate helpers like other file tools.
    #[must_use]
    pub fn with_media_options(mut self, media: MediaReadOptions) -> Self {
        self.media = media;
        self
    }

    /// Standalone JSON metadata for the five tools; no model-vendor contract is required.
    #[must_use]
    pub fn definitions() -> Vec<Value> {
        crate::portable_files::FileEngine::<Self>::definitions()
    }

    /// Alias for [`Self::definitions`].
    #[must_use]
    pub fn tool_schemas() -> Vec<Value> {
        Self::definitions()
    }

    /// Execute a text operation. Media requires [`Self::execute_output`]; it is never
    /// silently converted to text or discarded by this compatibility interface.
    pub async fn execute(&self, name: &str, input: Value) -> Result<String, String> {
        match self.execute_output(name, input).await?.content {
            ToolContent::Text(text) => Ok(text),
            ToolContent::Blocks(blocks) => {
                let mut text = String::new();
                for block in blocks {
                    match block {
                        crate::ToolResultBlock::Text { text: part } => text.push_str(&part),
                        _ => return Err("Read returned media; use execute_output to preserve native Claude content blocks".into()),
                    }
                }
                Ok(text)
            }
        }
    }

    /// Execute with native Claude text/image result blocks, preserving actual media.
    pub async fn execute_output(&self, name: &str, input: Value) -> Result<ToolOutput, String> {
        self.execute_output_with_context(name, input, true).await
    }

    /// Execute a file operation with optional project-guidance loading.
    ///
    /// Hosts enforcing read restrictions can disable augmentation to avoid
    /// opening unrelated instructions, rules or imports. This flag does not
    /// authorize the requested operation; the host must check that separately.
    pub async fn execute_output_with_context(
        &self,
        name: &str,
        input: Value,
        include_project_context: bool,
    ) -> Result<ToolOutput, String> {
        let this = self.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut media = None;
            if name == "Read" {
                let fields = input.as_object().ok_or("Read input must be an object")?;
                if let Some(key) = fields
                    .keys()
                    .find(|k| !["file_path", "offset", "limit", "pages"].contains(&k.as_str()))
                {
                    return Err(format!("unsupported Read option: {key}"));
                }
                let (_, path) = this.file(Self::field(&input, "file_path")?)?;
                media = crate::media::read(&path, &input, &this.media)?;
            }
            let mut output = match media {
                Some(output) => output,
                None => ToolOutput::text(this.execute_sync(&name, &input)?),
            };
            if !include_project_context {
                return Ok(output);
            }
            // Load guidance for the requested path only. Searching a directory
            // does not imply opening every descendant's instructions.
            let requested = if matches!(name.as_str(), "Glob" | "Grep") {
                input.get("path").and_then(Value::as_str).unwrap_or(".")
            } else {
                Self::field(&input, "file_path")?
            };
            let relative = this.relative(requested, true)?;
            let relative = if relative == Path::new(".") { Path::new("") } else { &relative };
            // Context diagnostics are data: a failed guidance read must never
            // disguise a successful Write/Edit as a failed mutation.
            if let Ok(loader) = crate::ClaudeProjectContext::new(&this.root) {
                let context = loader.load_for_path(relative);
                if !context.excerpts.is_empty() || !context.diagnostics.is_empty() {
                    let context = json!(context);
                    let text = format!("\nWorkspace context (guidance only; does not expand tool authority):\n{context}\n");
                    match &mut output.content {
                        ToolContent::Text(body) => body.push_str(&text),
                        ToolContent::Blocks(blocks) => blocks.push(crate::ToolResultBlock::Text { text }),
                    }
                    let metadata = output.metadata.get_or_insert_with(|| json!({}));
                    metadata["project_context"] = context;
                }
            }
            Ok(output)
        })
        .await
        .map_err(|e| format!("workspace task: {e}"))?
    }

    fn execute_sync(&self, name: &str, input: &Value) -> Result<String, String> {
        crate::portable_files::FileEngine(self).execute(name, input)
    }

    fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing or invalid {key}"))
    }

    fn relative(&self, text: &str, allow_root: bool) -> Result<PathBuf, String> {
        crate::portable_files::relative_path(&self.root, &self.root_alias, text, allow_root)
    }

    fn existing(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let rel = self.relative(text, true)?;
        let target = self.root.join(&rel);
        let real = fs::canonicalize(target).map_err(|e| format!("path: {e}"))?;
        if !real.starts_with(&self.root) {
            return Err("symlink escapes workspace".into());
        }
        Ok((rel, real))
    }

    fn file(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let (rel, real) = self.existing(text)?;
        if !real.is_file() {
            return Err("path is not a regular file".into());
        }
        Ok((rel, real))
    }

    fn read_text(path: &Path) -> Result<String, String> {
        let f = fs::File::open(path).map_err(|e| format!("read: {e}"))?;
        let mut bytes = Vec::new();
        f.take((MAX_FILE + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read: {e}"))?;
        if bytes.len() > MAX_FILE {
            return Err("file exceeds 1 MiB text limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "file is not UTF-8 text".into())
    }

    fn write_target(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let rel = self.relative(text, false)?;
        let path = self.root.join(&rel);
        let parent = path.parent().ok_or("file has no parent")?;
        let mut current = self.root.clone();
        for component in rel.parent().into_iter().flat_map(Path::components) {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err("symlink parent rejected".into());
                }
                Ok(meta) if !meta.is_dir() => return Err("parent is not a directory".into()),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current).map_err(|e| format!("create directory: {e}"))?
                }
                Err(e) => return Err(format!("check parent: {e}")),
            }
        }
        if fs::canonicalize(parent).map_err(|e| format!("parent: {e}"))? != parent {
            return Err("parent escapes workspace or uses a symlink".into());
        }
        reject_symlink_target(&path)?;
        Ok((rel, path))
    }

    fn walk(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        let mut stack = vec![path.to_path_buf()];
        let mut files = Vec::new();
        let mut visits = 0;
        while let Some(next) = stack.pop() {
            visits += 1;
            if visits > MAX_VISITS {
                return Err("search exceeds 10000 entries".into());
            }
            let meta = fs::symlink_metadata(&next).map_err(|e| format!("walk: {e}"))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_file() {
                files.push(next);
            } else if meta.is_dir() {
                for entry in fs::read_dir(&next).map_err(|e| format!("walk: {e}"))? {
                    // Count discovered entries before enqueuing, not only after
                    // popping: one wide directory must not bypass the memory cap.
                    if visits + stack.len() >= MAX_VISITS {
                        return Err("search exceeds 10000 entries".into());
                    }
                    stack.push(entry.map_err(|e| format!("walk: {e}"))?.path());
                }
            }
        }
        files.sort();
        Ok(files)
    }
}

fn reject_symlink_target(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err("symlink target rejected".into()),
        Ok(meta) if !meta.is_file() => Err("target is not a regular file".into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("check target: {e}")),
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("file has no parent")?;
    let mut temp = None;
    for _ in 0..16 {
        let name = format!(
            ".nanocodex-{}-{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        );
        let candidate = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create temp file: {e}")),
        }
    }
    let (temp_path, mut file) = temp.ok_or("could not allocate temp file")?;
    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        file.sync_all().map_err(|e| format!("sync: {e}"))?;
        drop(file);
        reject_symlink_target(path)?;
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&temp_path, meta.permissions())
                .map_err(|e| format!("permissions: {e}"))?;
        }
        fs::rename(&temp_path, path).map_err(|e| format!("rename: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

impl crate::portable_files::FileHost for ClaudeWorkspaceFiles {
    fn root(&self) -> &Path {
        &self.root
    }
    fn relative(&self, text: &str, allow_root: bool) -> Result<PathBuf, String> {
        self.relative(text, allow_root)
    }
    fn existing(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        self.existing(text)
    }
    fn file(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        self.file(text)
    }
    fn read_text(&self, path: &Path) -> Result<String, String> {
        Self::read_text(path)
    }
    fn write_target(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        self.write_target(text)
    }
    fn write(&self, path: &Path, content: &str) -> Result<(), String> {
        atomic_write(path, content)
    }
    fn walk(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        self.walk(path)
    }
    fn size(&self, path: &Path) -> Result<u64, String> {
        fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| format!("search metadata: {e}"))
    }
    fn modified(&self, path: &Path) -> Option<u128> {
        fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .into()
    }
    fn check_glob_prefix(&self, root: &Path, prefix: &str) -> Result<(), String> {
        let candidate = root.join(prefix);
        if !prefix.is_empty()
            && candidate.exists()
            && !fs::canonicalize(candidate)
                .map_err(|e| format!("glob path: {e}"))?
                .starts_with(&self.root)
        {
            return Err("glob symlink escapes workspace".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ClaudeWorkspaceFiles;
    use serde_json::json;
    use std::fs;

    #[tokio::test]
    async fn mutation_options_and_expansion_fail_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        let path = dir.path().join("original");
        fs::write(&path, "aa").unwrap();
        for (name, input) in [
            (
                "Write",
                json!({"file_path":"original","content":"bad","append":true}),
            ),
            (
                "Edit",
                json!({"file_path":"original","old_string":"aa","new_string":"bad","replaceAll":true}),
            ),
            (
                "Edit",
                json!({"file_path":"original","old_string":"a","new_string":"x".repeat(super::MAX_FILE),"replace_all":true}),
            ),
        ] {
            assert!(
                files.execute(name, input).await.is_err(),
                "{name} accepted invalid mutation"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "aa");
        }
        files.execute("Edit", json!({"file_path":"original","old_string":"a","new_string":"é".repeat(super::MAX_FILE / 4),"replace_all":true})).await.unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), super::MAX_FILE as u64);
    }

    #[tokio::test]
    async fn scoped_search_and_unicode_glob() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::write(dir.path().join("src/nested/é.rs"), "needle").unwrap();
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"path":"src","glob":"nested/*.rs","pattern":"needle"})
                )
                .await
                .unwrap(),
            "src/nested/é.rs\n"
        );
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"path":"src/nested/é.rs","glob":"*.rs","pattern":"needle"})
                )
                .await
                .unwrap(),
            "src/nested/é.rs\n"
        );
        assert_eq!(
            files
                .execute("Glob", json!({"path":"src","pattern":"**/é.rs"}))
                .await
                .unwrap(),
            "src/nested/é.rs\n"
        );
    }

    #[tokio::test]
    async fn round_trip_and_exact_edit() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        files
            .execute(
                "Write",
                json!({"file_path":"a/b.txt","content":"red\nblue\n"}),
            )
            .await
            .unwrap();
        assert_eq!(
            files
                .execute("Read", json!({"file_path":"a/b.txt"}))
                .await
                .unwrap(),
            "1\tred\n2\tblue\n"
        );
        assert!(
            files
                .execute(
                    "Edit",
                    json!({"file_path":"a/b.txt","old_string":"red","new_string":"green"})
                )
                .await
                .is_ok()
        );
        assert!(
            files
                .execute("Read", json!({"file_path":"a/b.txt"}))
                .await
                .unwrap()
                .contains("green")
        );
        assert!(
            files
                .execute("Glob", json!({"pattern":"**/*.txt"}))
                .await
                .unwrap()
                .contains("a/b.txt")
        );
        assert!(
            files
                .execute("Grep", json!({"pattern":"green","output_mode":"content"}))
                .await
                .unwrap()
                .contains("a/b.txt:1:green")
        );
    }

    #[tokio::test]
    async fn grep_uses_bounded_regular_expressions() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        files
            .execute(
                "Write",
                json!({"file_path":"a.txt","content":"Alpha 42\nbeta 99\n"}),
            )
            .await
            .unwrap();
        let found = files
            .execute(
                "Grep",
                json!({"pattern":"^alpha [0-9]+$","-i":true,"output_mode":"content"}),
            )
            .await
            .unwrap();
        assert_eq!(found, "a.txt:1:Alpha 42\n");
        assert!(files.execute("Grep", json!({"pattern":"["})).await.is_err());
    }

    #[tokio::test]
    async fn grep_modes_filters_pagination_and_context() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        files
            .execute(
                "Write",
                json!({"file_path":"src/a.rs","content":"before\nHit Hit\nafter\nspacer\nHit\n"}),
            )
            .await
            .unwrap();
        files
            .execute("Write", json!({"file_path":"src/b.txt","content":"hit\n"}))
            .await
            .unwrap();
        files
            .execute("Write", json!({"file_path":"src/c.rs","content":"hit\n"}))
            .await
            .unwrap();
        assert_eq!(
            files
                .execute("Grep", json!({"pattern":"Hit", "glob":"*.rs"}))
                .await
                .unwrap(),
            "src/a.rs\n"
        );
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"pattern":"hit", "-i":true,"glob":"*.rs","output_mode":"count"})
                )
                .await
                .unwrap(),
            "src/a.rs:2\nsrc/c.rs:1\n"
        );
        assert_eq!(files.execute("Grep", json!({"pattern":"hit", "-i":true,"output_mode":"files_with_matches","offset":1,"head_limit":1})).await.unwrap(),
            "src/b.txt\n");
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"pattern":"Hit", "output_mode":"content","-C":1,"head_limit":1})
                )
                .await
                .unwrap(),
            "src/a.rs-1-before\nsrc/a.rs:2:Hit Hit\nsrc/a.rs-3-after\n"
        );
        assert_eq!(files.execute("Grep", json!({"pattern":"Hit", "output_mode":"content","-o":true,"-n":false,"head_limit":2})).await.unwrap(),
            "src/a.rs:Hit\nsrc/a.rs:Hit\n");
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"pattern":"Hit", "output_mode":"content","offset":1,"head_limit":1})
                )
                .await
                .unwrap(),
            "src/a.rs:5:Hit\n"
        );
    }

    #[tokio::test]
    async fn grep_multiline_and_rejected_options() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        files
            .execute(
                "Write",
                json!({"file_path":"a.txt","content":"start\nmiddle\nend\n"}),
            )
            .await
            .unwrap();
        assert_eq!(
            files
                .execute(
                    "Grep",
                    json!({"pattern":"start.*end","multiline":true,"output_mode":"content"})
                )
                .await
                .unwrap(),
            "a.txt:1:start\na.txt:2:middle\na.txt:3:end\n"
        );
        for bad in [
            json!({"pattern":"x", "output_mode":"content", "-o":true,"multiline":true}),
            json!({"pattern":"x", "-A":-1}),
            json!({"pattern":"x", "head_limit":"10"}),
        ] {
            assert!(files.execute("Grep", bad).await.is_err());
        }
        assert!(
            files
                .execute("Read", json!({"file_path":"a.txt", "pages":"1-2"}))
                .await
                .is_err()
        );
        let schema = ClaudeWorkspaceFiles::definitions();
        let grep = schema.iter().find(|s| s["name"] == "Grep").unwrap();
        assert!(grep["input_schema"]["properties"].get("type").is_some());
        assert!(
            grep["input_schema"]["properties"]
                .get("multiline")
                .is_some()
        );
        assert!(
            schema.iter().find(|s| s["name"] == "Read").unwrap()["input_schema"]["properties"]
                .get("pages")
                .is_some()
        );
    }

    #[tokio::test]
    async fn reject_escape_and_ambiguous_edit() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "private").unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        for path in [
            "../secret.txt",
            outside.path().join("secret.txt").to_str().unwrap(),
        ] {
            assert!(
                files
                    .execute("Read", json!({"file_path": path}))
                    .await
                    .is_err()
            );
            assert!(
                files
                    .execute("Write", json!({"file_path": path,"content":"bad"}))
                    .await
                    .is_err()
            );
        }
        files
            .execute("Write", json!({"file_path":"dupe","content":"aa aa"}))
            .await
            .unwrap();
        assert!(
            files
                .execute(
                    "Edit",
                    json!({"file_path":"dupe","old_string":"aa","new_string":"bb"})
                )
                .await
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("dupe")).unwrap(),
            "aa aa"
        );
        files
            .execute(
                "Edit",
                json!({"file_path":"dupe","old_string":"aa","new_string":"bb","replace_all":true}),
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("dupe")).unwrap(),
            "bb bb"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_blocked_for_every_operation() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "private").unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        assert!(
            files
                .execute("Read", json!({"file_path":"link/secret"}))
                .await
                .is_err()
        );
        assert!(
            files
                .execute("Write", json!({"file_path":"link/new","content":"bad"}))
                .await
                .is_err()
        );
        assert!(
            files
                .execute("Glob", json!({"pattern":"link/**"}))
                .await
                .is_err()
        );
        assert!(
            files
                .execute("Grep", json!({"pattern":"private","path":"link"}))
                .await
                .is_err()
        );
        assert!(!outside.path().join("new").exists());
    }

    #[tokio::test]
    async fn absolute_inside_root_and_output_limits() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        let path = dir.path().join("inside.txt");
        files
            .execute(
                "Write",
                json!({"file_path":path,"content":"hit".repeat(50_000)}),
            )
            .await
            .unwrap();
        let read = files
            .execute("Read", json!({"file_path":path,"limit":50_000}))
            .await
            .unwrap();
        assert!(read.len() <= 64 * 1024);
        assert!(read.contains("[output truncated]"));
        let grep = files
            .execute(
                "Grep",
                json!({"pattern":"hit","path":path,"output_mode":"content"}),
            )
            .await
            .unwrap();
        assert!(grep.len() <= 64 * 1024);
        assert!(
            files
                .execute(
                    "Glob",
                    json!({"pattern":format!("{}/*.txt",dir.path().display())})
                )
                .await
                .unwrap()
                .contains("inside.txt")
        );
    }

    #[tokio::test]
    async fn limits_and_invalid_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
        assert!(
            files
                .execute(
                    "Write",
                    json!({"file_path":"huge","content":"x".repeat(1_048_577)})
                )
                .await
                .is_err()
        );
        assert!(
            files
                .execute("Read", json!({"file_path":"missing"}))
                .await
                .is_err()
        );
        assert!(
            files
                .execute("Glob", json!({"pattern":"../**"}))
                .await
                .is_err()
        );
        assert!(files.execute("Nope", json!({})).await.is_err());
        assert_eq!(ClaudeWorkspaceFiles::tool_schemas().len(), 5);
    }
}
