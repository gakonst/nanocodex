//! Bounded native text filesystem adapter. The embedding explicitly selects a
//! root. Do not share that root with hostile processes: pathname checks cannot
//! prevent concurrent symlink/rename races; OS isolation belongs to the host.
use crate::host::*;
use globset::{GlobBuilder, GlobMatcher};
use regex::RegexBuilder;
use serde_json::json;
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
const MAX_FILE: usize = 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_ENTRIES: usize = 10_000;
static TEMP: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Debug)]
pub struct XaiWorkspaceFiles {
    root: PathBuf,
    lock: Arc<Mutex<()>>,
}
impl XaiWorkspaceFiles {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        if !root.is_dir() {
            return Err("workspace root is not a directory".into());
        }
        Ok(Self {
            root,
            lock: Arc::new(Mutex::new(())),
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn definitions() -> Vec<ToolDefinition> {
        vec![
            definition(
                "read_file",
                "Read a bounded UTF-8 workspace file with 1-based line anchors. Binary/PDF/image extraction is not installed.",
                json!({"target_file":{"type":"string"},"offset":{"type":"integer","minimum":1,"default":1},"limit":{"type":"integer","minimum":1,"maximum":1000,"default":1000}}),
                &["target_file"],
            ),
            definition(
                "write",
                "Create or replace a UTF-8 workspace file atomically; create parent directories.",
                json!({"file_path":{"type":"string"},"content":{"type":"string"}}),
                &["file_path", "content"],
            ),
            definition(
                "search_replace",
                "Replace exact text; require one match unless replace_all. Empty old_string creates or replaces the whole file.",
                json!({"file_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean","default":false}}),
                &["file_path", "old_string", "new_string"],
            ),
            definition(
                "list_dir",
                "List a bounded directory tree, breadth first; symlinks are excluded.",
                json!({"target_directory":{"type":"string"}}),
                &["target_directory"],
            ),
            definition(
                "glob",
                "Find workspace files by glob pattern; symlinks are excluded.",
                json!({"pattern":{"type":"string"},"path":{"type":"string"}}),
                &["pattern"],
            ),
            definition(
                "grep",
                "Search bounded UTF-8 workspace files with Rust regular expressions. Return content, paths or counts. Symlinks and binary files are excluded.",
                json!({"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"},"output_mode":{"type":"string","enum":["content","files_with_matches","count"],"default":"content"},"-A":{"type":"integer","minimum":0,"maximum":100},"-B":{"type":"integer","minimum":0,"maximum":100},"-C":{"type":"integer","minimum":0,"maximum":100},"-i":{"type":"boolean"},"head_limit":{"type":"integer","minimum":0,"maximum":1000},"offset":{"type":"integer","minimum":0},"multiline":{"type":"boolean"}}),
                &["pattern"],
            ),
        ]
    }
    fn path(&self, text: &str, allow_root: bool) -> Result<PathBuf, String> {
        if text.len() > 4096 {
            return Err("path too long".into());
        }
        let path = Path::new(text);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| "path outside workspace")?
        } else {
            path
        };
        let mut result = self.root.clone();
        for c in relative.components() {
            match c {
                Component::Normal(c) => result.push(c),
                Component::CurDir => {}
                _ => return Err("path traversal outside workspace is forbidden".into()),
            };
            match fs::symlink_metadata(&result) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err("symlinks are excluded from workspace tools".into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        if result == self.root && !allow_root {
            return Err("file path must name a file".into());
        }
        Ok(result)
    }
    fn read_text(&self, path: &Path) -> Result<String, String> {
        if !fs::symlink_metadata(path)
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err("path is not a regular file".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A same-root replacement with a FIFO must never block opening it.
            options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("path is not a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_FILE + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE {
            return Err("file exceeds 1 MiB text limit".into());
        }
        if bytes.contains(&0) {
            return Err("binary file is unsupported".into());
        }
        String::from_utf8(bytes).map_err(|_| "file is not UTF-8".into())
    }
    fn write_text(&self, path: &Path, text: &str) -> Result<(), String> {
        if text.len() > MAX_FILE {
            return Err("file exceeds 1 MiB text limit".into());
        }
        let parent = path.parent().ok_or("file has no parent")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        self.path(path.to_str().ok_or("path is not UTF-8")?, false)?;
        let permissions = match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
            Ok(_) => return Err("path is not a regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let temp = parent.join(format!(
            ".xai-write-{}-{}",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let outcome = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&temp).map_err(|e| e.to_string())?;
            created = true;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            if let Some(permissions) = permissions {
                f.set_permissions(permissions).map_err(|e| e.to_string())?;
            }
            f.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&temp, path).map_err(|e| e.to_string())
        })();
        if outcome.is_err() && created {
            let _ = fs::remove_file(&temp);
        }
        outcome
    }
    fn walk(&self, start: &Path) -> Result<(Vec<PathBuf>, bool), String> {
        if start.is_file() {
            return Ok((vec![start.to_path_buf()], false));
        }
        let mut queue = VecDeque::from([start.to_path_buf()]);
        let mut paths = vec![];
        while let Some(dir) = queue.pop_front() {
            let mut entries = vec![];
            for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                if paths.len() + entries.len() >= MAX_ENTRIES {
                    return Ok((paths, true));
                }
                entries.push(entry.map_err(|e| e.to_string())?);
            }
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let ty = entry.file_type().map_err(|e| e.to_string())?;
                if ty.is_symlink() {
                    continue;
                }
                if ty.is_dir() {
                    queue.push_back(entry.path());
                }
                if ty.is_dir() || ty.is_file() {
                    paths.push(entry.path());
                }
            }
        }
        Ok((paths, false))
    }
    fn run(&self, request: HostRequest) -> Result<ToolOutput, String> {
        validate_request(&request)?;
        let _guard = self.lock.lock().map_err(|_| "workspace lock poisoned")?;
        let input = &request.input;
        let mut truncated = false;
        let mut skipped_files = 0;
        let (text, data) = match request.tool.as_str() {
            "read_file" => {
                fields(input, &["target_file", "offset", "limit"])?;
                let path = self.path(string(input, "target_file")?, false)?;
                let offset = number(input, "offset", 1, u32::MAX as u64)? as usize;
                let limit = number(input, "limit", 1000, 1000)? as usize;
                if offset == 0 || limit == 0 {
                    return Err("offset and limit must be positive".into());
                }
                let content = self.read_text(&path)?;
                let lines: Vec<_> = content.lines().collect();
                let text = lines
                    .iter()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, line)| format!("{}→{}", i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                truncated = offset - 1 + limit < lines.len();
                (
                    text,
                    json!({"path":path,"total_lines":lines.len(),"offset":offset,"limit":limit}),
                )
            }
            "write" | "search_replace" => {
                let path = self.path(string(input, "file_path")?, false)?;
                let (content, replacements) = if request.tool == "write" {
                    fields(input, &["file_path", "content"])?;
                    (string(input, "content")?.to_owned(), 0)
                } else {
                    fields(
                        input,
                        &["file_path", "old_string", "new_string", "replace_all"],
                    )?;
                    let old = string(input, "old_string")?;
                    let new = string(input, "new_string")?;
                    if old == new {
                        return Err("old_string and new_string must differ".into());
                    }
                    let all = boolean(input, "replace_all", false)?;
                    if old.is_empty() {
                        (new.to_owned(), 0)
                    } else {
                        let before = self.read_text(&path)?;
                        let count = before.matches(old).count();
                        if count == 0 {
                            return Err(
                                "old_string not found; reread the file before retrying".into()
                            );
                        }
                        if count > 1 && !all {
                            return Err(
                                "old_string is ambiguous; add context or set replace_all".into()
                            );
                        }
                        (
                            if all {
                                before.replace(old, new)
                            } else {
                                before.replacen(old, new, 1)
                            },
                            count,
                        )
                    }
                };
                self.write_text(&path, &content)?;
                (
                    format!("Wrote {} bytes to {}", content.len(), path.display()),
                    json!({"path":path,"bytes_written":content.len(),"replacements":replacements}),
                )
            }
            "list_dir" | "glob" | "grep" => {
                let key = if request.tool == "list_dir" {
                    "target_directory"
                } else {
                    "path"
                };
                let root = self.path(
                    input
                        .get(key)
                        .map(|_| string(input, key))
                        .transpose()?
                        .unwrap_or("."),
                    true,
                )?;
                if request.tool == "list_dir" {
                    fields(input, &["target_directory"])?;
                    string(input, "target_directory")?;
                    if !root.is_dir() {
                        return Err("target_directory is not a directory".into());
                    }
                }
                if request.tool == "glob" {
                    fields(input, &["path", "pattern"])?;
                }
                let (paths, cut) = self.walk(&root)?;
                truncated = cut;
                let mut results = vec![];
                if request.tool == "grep" {
                    fields(
                        input,
                        &[
                            "pattern",
                            "path",
                            "glob",
                            "output_mode",
                            "-A",
                            "-B",
                            "-C",
                            "-i",
                            "head_limit",
                            "offset",
                            "multiline",
                        ],
                    )?;
                    let pattern = string(input, "pattern")?;
                    if pattern.len() > 4096 {
                        return Err("regex too large".into());
                    }
                    let multiline = boolean(input, "multiline", false)?;
                    let regex = RegexBuilder::new(pattern)
                        .case_insensitive(boolean(input, "-i", false)?)
                        .multi_line(multiline)
                        .dot_matches_new_line(multiline)
                        .size_limit(1024 * 1024)
                        .build()
                        .map_err(|e| e.to_string())?;
                    let filter = input
                        .get("glob")
                        .map(|_| glob(string(input, "glob")?))
                        .transpose()?;
                    let mode = input["output_mode"].as_str().unwrap_or("content");
                    if !["content", "files_with_matches", "count"].contains(&mode) {
                        return Err("invalid output_mode".into());
                    }
                    let context = number(input, "-C", 0, 100)?;
                    let before = number(input, "-B", context, 100)? as usize;
                    let after = number(input, "-A", context, 100)? as usize;
                    let offset = number(input, "offset", 0, 100_000)? as usize;
                    let limit = number(input, "head_limit", 200, 1000)? as usize;
                    let limit = if limit == 0 { 1000 } else { limit };
                    let mut read_bytes = 0;
                    let mut seen = 0;
                    'search: for path in paths.iter().filter(|p| p.is_file()) {
                        let rel = path.strip_prefix(&self.root).unwrap();
                        if filter.as_ref().is_some_and(|g| {
                            !g.is_match(rel) && !g.is_match(path.file_name().unwrap_or_default())
                        }) {
                            continue;
                        }
                        let content = match self.read_text(path) {
                            Ok(s) => s,
                            Err(_) => {
                                skipped_files += 1;
                                continue;
                            }
                        };
                        read_bytes += content.len();
                        if read_bytes > 16 * 1024 * 1024 {
                            truncated = true;
                            break;
                        }
                        let matches: Vec<_> = regex.find_iter(&content).take(10_001).collect();
                        if matches.len() > 10_000 {
                            truncated = true;
                        }
                        if matches.is_empty() {
                            continue;
                        }
                        let entries = match mode {
                            "files_with_matches" => vec![rel.display().to_string()],
                            "count" => vec![format!("{}:{}", rel.display(), matches.len())],
                            _ => {
                                let lines: Vec<_> = content.lines().collect();
                                let mut selected = std::collections::BTreeSet::new();
                                for m in matches.iter().take(10_000) {
                                    let a = content[..m.start()]
                                        .bytes()
                                        .filter(|&b| b == b'\n')
                                        .count();
                                    let b =
                                        content[..m.end()].bytes().filter(|&b| b == b'\n').count();
                                    for i in a.saturating_sub(before)
                                        ..=(b + after).min(lines.len().saturating_sub(1))
                                    {
                                        selected.insert(i);
                                    }
                                }
                                selected
                                    .into_iter()
                                    .map(|i| {
                                        format!(
                                            "{}:{}:{}",
                                            rel.display(),
                                            i + 1,
                                            lines.get(i).unwrap_or(&"")
                                        )
                                    })
                                    .collect()
                            }
                        };
                        for entry in entries {
                            seen += 1;
                            if seen <= offset {
                                continue;
                            }
                            if results.len() >= limit {
                                truncated = true;
                                break 'search;
                            }
                            results.push(entry);
                        }
                    }
                } else {
                    let matcher = if request.tool == "glob" {
                        Some(glob(string(input, "pattern")?)?)
                    } else {
                        None
                    };
                    for path in &paths {
                        let rel = path.strip_prefix(&self.root).unwrap();
                        if let Some(matcher) = &matcher
                            && (!path.is_file()
                                || (!matcher.is_match(path.strip_prefix(&root).unwrap_or(rel))
                                    && !matcher.is_match(path.file_name().unwrap_or_default())))
                        {
                            continue;
                        }
                        results.push(format!(
                            "{}{}",
                            rel.display(),
                            if path.is_dir() { "/" } else { "" }
                        ));
                        if results.iter().map(String::len).sum::<usize>() > MAX_OUTPUT {
                            truncated = true;
                            break;
                        }
                    }
                }
                (
                    results.join("\n"),
                    json!({"entries":results.len(),"skipped_files":skipped_files}),
                )
            }
            _ => return Err(format!("workspace tool not installed: {}", request.tool)),
        };
        let (mut text, cut) = cap(&text, MAX_OUTPUT);
        truncated |= cut;
        if truncated {
            text.push_str("\n[Output truncated; narrow the path or use a smaller window.]");
        }
        Ok(ToolOutput::text(text).with_structured_result(data).with_metadata(json!({"call_id":request.context.call_id,"truncated":truncated,"skipped_files":skipped_files})))
    }
}
impl XaiHost for XaiWorkspaceFiles {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Self::definitions()
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.run(request))
                .await
                .map_err(|e| e.to_string())?
        })
    }
}
fn glob(pattern: &str) -> Result<GlobMatcher, String> {
    if pattern.len() > 4096 {
        return Err("glob too large".into());
    }
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| e.to_string())
}
pub(crate) fn cap(text: &str, max: usize) -> (String, bool) {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), end < text.len())
}
