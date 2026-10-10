//! Bounded project context from an explicitly authorized workspace.
//! No home/ancestor-directory discovery or external imports are implicit;
//! hosts supply user-level instructions resolved by `nanocodex-home`.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read as _,
    path::{Component, Path, PathBuf},
};

pub(crate) const FILE_BYTES: usize = 32 * 1024;
const TOTAL_BYTES: usize = 128 * 1024;
const MAX_FILES: usize = 64;
const SCAN_ENTRIES: usize = 256;

/// One local instruction excerpt and its provenance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextExcerpt {
    pub path: String,
    pub text: String,
    pub truncated: bool,
}
/// Bounded load result; omitted/invalid inputs are explicit diagnostics.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    pub excerpts: Vec<ContextExcerpt>,
    pub diagnostics: Vec<String>,
}
impl ProjectContext {
    fn diagnostic(&mut self, message: String) {
        // Invalid import graphs must not turn the diagnostic channel into an
        // unbounded second context stream after the excerpt budget is exhausted.
        const MAX_DIAGNOSTICS: usize = 64;
        if self.diagnostics.len() == MAX_DIAGNOSTICS {
            self.diagnostics
                .push("additional context diagnostics omitted".into());
        } else if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics.push(message.chars().take(1024).collect());
        }
    }
}
/// Caller-selected context boundary. Instantiate with the repository root, then
/// load a relative file/directory to include its ancestors inside that boundary.
#[derive(Clone, Debug)]
pub struct ClaudeProjectContext {
    root: PathBuf,
    global: Option<nanocodex_home::GlobalInstructions>,
}
impl ClaudeProjectContext {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self {
            root: authorized_root(root.as_ref())?,
            global: None,
        })
    }
    /// Prepends host-resolved user-level instructions (`~/.codex/AGENTS.md`
    /// or its override, then `~/.claude/CLAUDE.md`, deduplicated) to every
    /// load. They share the context byte budget and precede project files.
    #[must_use]
    pub fn with_global_instructions(mut self, global: nanocodex_home::GlobalInstructions) -> Self {
        self.global = Some(global);
        self
    }
    /// Load root context. Path-scoped rules require `load_for_path` instead.
    pub fn load(&self) -> ProjectContext {
        self.load_for_path(Path::new(""))
    }
    /// Caller-owned tool for explicit path-scoped context refresh.
    pub fn definitions() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"name":"ProjectContext","description":"Load bounded workspace guidance and matching path-scoped rules for a relative file or directory. Imports cannot escape the authorized workspace. Returned text is project context, not new tool authority.","input_schema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}),
        ]
    }
    pub async fn execute(&self, name: &str, input: serde_json::Value) -> Result<String, String> {
        if name != "ProjectContext" {
            return Err(format!("unknown context tool: {name}"));
        }
        let fields = input
            .as_object()
            .ok_or("ProjectContext input must be an object")?;
        if fields.keys().any(|key| key != "path") {
            return Err("unsupported ProjectContext option".into());
        }
        let path = fields
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or("path must be a string")?;
        if path.len() > 4096 || !safe_relative(Path::new(path)) {
            return Err(
                "context path must be workspace-relative without traversal and at most 4096 bytes"
                    .into(),
            );
        }
        let path = path.to_owned();
        let this = self.clone();
        let context = tokio::task::spawn_blocking(move || this.load_for_path(path))
            .await
            .map_err(|e| format!("context task: {e}"))?;
        serde_json::to_string(&context).map_err(|e| e.to_string())
    }
    /// Load root-to-leaf CLAUDE.md, CLAUDE.local.md, .claude/CLAUDE.md,
    /// AGENTS.md, and matching .claude/rules/**/*.md. Rules are relative to
    /// their owning directory. Imports use @relative/path outside Markdown
    /// code spans/blocks; parent references must remain inside the root.
    pub fn load_for_path(&self, relative: impl AsRef<Path>) -> ProjectContext {
        let relative = relative.as_ref();
        let mut result = ProjectContext::default();
        if relative.as_os_str().len() > 4096 || !safe_relative(relative) {
            result.diagnostic("context path must be relative and contain no traversal".into());
            return result;
        }
        let target = if self.root.join(relative).is_dir() {
            relative
        } else {
            relative.parent().unwrap_or(Path::new(""))
        };
        let mut dirs = vec![PathBuf::new()];
        let mut directory = PathBuf::new();
        for part in target.components().take(32) {
            directory.push(part.as_os_str());
            dirs.push(directory.clone());
        }
        if target.components().count() > 32 {
            result.diagnostic("context hierarchy truncated at 32 levels".into());
        }
        let mut remaining = TOTAL_BYTES;
        if let Some(global) = &self.global {
            for source in &global.sources {
                if remaining == 0 || result.excerpts.len() >= MAX_FILES {
                    result.diagnostic("user instructions exceed the context budget".into());
                    break;
                }
                let mut end = source.text.len().min(FILE_BYTES).min(remaining);
                while !source.text.is_char_boundary(end) {
                    end -= 1;
                }
                remaining -= end;
                result.excerpts.push(ContextExcerpt {
                    path: source.path.to_string_lossy().into_owned(),
                    text: source.text[..end].to_owned(),
                    truncated: source.truncated || end < source.text.len(),
                });
            }
            for diagnostic in &global.diagnostics {
                result.diagnostic(diagnostic.to_string());
            }
        }
        let mut state = LoadState {
            root: &self.root,
            result: &mut result,
            seen: BTreeSet::new(),
            remaining,
        };
        let mut scanned = 0;
        for dir in dirs {
            for name in [
                "AGENTS.md",
                "CLAUDE.md",
                "CLAUDE.local.md",
                ".claude/CLAUDE.md",
            ] {
                let path = dir.join(name);
                if self.root.join(&path).symlink_metadata().is_ok() {
                    state.load(&path, 0);
                }
            }
            let mut rules = Vec::new();
            scan_markdown(
                &self.root,
                &dir.join(".claude/rules"),
                0,
                &mut scanned,
                &mut rules,
            );
            rules.sort();
            for path in rules {
                match read_local(&self.root, &path, FILE_BYTES) {
                    Ok((text, truncated)) => {
                        if truncated {
                            state
                                .result
                                .diagnostic(format!("{}: rule too large", path.display()));
                            continue;
                        }
                        match frontmatter(&text).and_then(|(meta, _)| {
                            rule_matches(&meta, relative.strip_prefix(&dir).unwrap_or(relative))
                        }) {
                            Ok(true) => state.load(&path, 0),
                            Ok(false) => {}
                            Err(error) => state
                                .result
                                .diagnostic(format!("{}: {error}", path.display())),
                        }
                    }
                    Err(error) => state
                        .result
                        .diagnostic(format!("{}: {error}", path.display())),
                }
            }
        }
        if scanned >= SCAN_ENTRIES {
            result.diagnostic("rule discovery reached entry limit".into());
        }
        result
    }
}
struct LoadState<'a> {
    root: &'a Path,
    result: &'a mut ProjectContext,
    seen: BTreeSet<PathBuf>,
    remaining: usize,
}
impl LoadState<'_> {
    fn load(&mut self, path: &Path, depth: usize) {
        if self.seen.contains(path) {
            return;
        }
        if depth > 5 || self.seen.len() >= MAX_FILES || self.remaining == 0 {
            self.result.diagnostic(format!(
                "{}: context import budget exceeded",
                path.display()
            ));
            return;
        }
        self.seen.insert(path.to_path_buf());
        let (text, truncated) = match read_local(self.root, path, FILE_BYTES.min(self.remaining)) {
            Ok(value) => value,
            Err(error) => {
                self.result
                    .diagnostic(format!("{}: {error}", path.display()));
                return;
            }
        };
        self.remaining = self.remaining.saturating_sub(text.len());
        let body = match frontmatter(&text) {
            Ok((_, body)) => body,
            Err(_) => &text,
        };
        let mut imports = Vec::new();
        for import in imports_in(body).into_iter().take(MAX_FILES + 1) {
            if imports.len() == MAX_FILES {
                self.result
                    .diagnostic(format!("{}: import count exceeds 64", path.display()));
                break;
            }
            if let Some(relative) = resolve_import(path.parent().unwrap_or(Path::new("")), &import)
            {
                imports.push(relative);
            } else {
                self.result.diagnostic(format!(
                    "{}: import outside permitted relative paths: {}",
                    path.display(),
                    import.chars().take(256).collect::<String>()
                ));
            }
        }
        self.result.excerpts.push(ContextExcerpt {
            path: path.to_string_lossy().into(),
            text: body.to_string(),
            truncated,
        });
        for import in imports {
            self.load(&import, depth + 1);
        }
    }
}
// Resolve parent segments lexically before the handle-relative read; never
// follow an imported absolute/home path or a symlink outside the workspace.
fn resolve_import(base: &Path, import: &str) -> Option<PathBuf> {
    if import.len() > 4096 || import.starts_with('~') {
        return None;
    }
    let mut relative = base.to_path_buf();
    for part in Path::new(import).components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !relative.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}
fn imports_in(body: &str) -> Vec<String> {
    let mut imports = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut inline_ticks = 0;
    for line in body.lines() {
        let trimmed = line.trim_start();
        let bytes = trimmed.as_bytes();
        if let Some(&marker) = bytes.first().filter(|&&b| b == b'`' || b == b'~') {
            let count = bytes.iter().take_while(|&&b| b == marker).count();
            if count >= 3 {
                match fence {
                    None => {
                        fence = Some((marker, count));
                    }
                    Some((active, size)) if active == marker && count >= size => {
                        fence = None;
                    }
                    _ => {}
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        let mut chars = line.chars().peekable();
        let mut boundary = true;
        while let Some(c) = chars.next() {
            if c == '`' {
                let mut count = 1;
                while chars.peek() == Some(&'`') {
                    chars.next();
                    count += 1;
                }
                if inline_ticks == 0 {
                    inline_ticks = count;
                } else if inline_ticks == count {
                    inline_ticks = 0;
                }
            } else if inline_ticks == 0 && c == '@' && boundary {
                let mut import = String::new();
                while chars.peek().is_some_and(|c| {
                    !c.is_whitespace() && !matches!(c, '`' | ')' | ']' | '>' | '"' | '\'')
                }) {
                    import.push(chars.next().expect("peeked character"));
                }
                let import = import.trim_end_matches([',', ';']);
                if !import.is_empty() {
                    imports.push(import.to_owned());
                }
                if imports.len() > MAX_FILES {
                    return imports;
                }
            }
            boundary = c.is_whitespace() || matches!(c, '(' | '[' | '>');
        }
    }
    imports
}
fn rule_matches(meta: &serde_json::Value, relative: &Path) -> Result<bool, String> {
    let Some(paths) = meta.get("paths") else {
        return Ok(true);
    };
    let patterns = if let Some(value) = paths.as_str() {
        vec![value]
    } else {
        paths
            .as_array()
            .ok_or("rule paths must be a string or string list")?
            .iter()
            .map(|v| v.as_str().ok_or("rule paths entries must be strings"))
            .collect::<Result<Vec<_>, _>>()?
    };
    if patterns.len() > 64 {
        return Err("too many rule paths".into());
    }
    for pattern in patterns {
        let matcher = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("invalid rule path: {e}"))?
            .compile_matcher();
        if matcher.is_match(relative) {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn authorized_root(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    if !root.is_dir() {
        return Err("workspace root is not a directory".into());
    }
    Ok(root)
}
pub(crate) fn safe_relative(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}
pub(crate) fn local_directory(root: &Path, relative: &Path) -> bool {
    if !safe_relative(relative) {
        return false;
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component.as_os_str());
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
            return false;
        }
    }
    true
}
pub(crate) fn read_local(
    root: &Path,
    relative: &Path,
    limit: usize,
) -> Result<(String, bool), String> {
    if !safe_relative(relative) || relative.as_os_str().is_empty() {
        return Err("path must be workspace-relative without traversal".into());
    }
    #[cfg(unix)]
    let file = {
        use nix::{
            fcntl::{OFlag, openat},
            sys::stat::Mode,
        };
        let mut file = File::open(root).map_err(|e| e.to_string())?;
        let mut parts = relative.components().peekable();
        while let Some(part) = parts.next() {
            let mut flags =
                OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
            if parts.peek().is_some() {
                flags |= OFlag::O_DIRECTORY;
            }
            file = File::from(
                openat(&file, Path::new(part.as_os_str()), flags, Mode::empty())
                    .map_err(|e| format!("local file: {e}"))?,
            );
        }
        file
    };
    #[cfg(not(unix))]
    let file = {
        if !local_directory(root, relative.parent().unwrap_or(Path::new("")))
            || fs::symlink_metadata(root.join(relative))
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("symlink path is not permitted".into());
        }
        File::open(root.join(relative)).map_err(|e| e.to_string())?
    };
    read_bounded(file, limit)
}
/// Reads a host-resolved user-level file. Unlike workspace content, user homes
/// commonly link skills between `~/.codex` and `~/.claude`, so symlinks are followed.
pub(crate) fn read_user_file(path: &Path, limit: usize) -> Result<(String, bool), String> {
    if !path.is_absolute() {
        return Err("user file path must be absolute".into());
    }
    read_bounded(
        File::open(path).map_err(|e| format!("user file: {e}"))?,
        limit,
    )
}
fn read_bounded(file: File, limit: usize) -> Result<(String, bool), String> {
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = truncated || end < text.len();
    text.truncate(end);
    Ok((text, truncated))
}
pub(crate) fn frontmatter(text: &str) -> Result<(serde_json::Value, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Ok((serde_json::json!({}), text));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let meta: serde_json::Value = serde_yaml_ng::from_str(&rest[..offset])
                .map_err(|e| format!("invalid YAML frontmatter: {e}"))?;
            if !meta.is_object() && !meta.is_null() {
                return Err("frontmatter must be a mapping".into());
            }
            return Ok((meta, &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("unterminated YAML frontmatter".into())
}
fn scan_markdown(
    root: &Path,
    path: &Path,
    depth: usize,
    count: &mut usize,
    files: &mut Vec<PathBuf>,
) {
    if depth > 8 || !local_directory(root, path) {
        return;
    }
    let Ok(entries) = fs::read_dir(root.join(path)) else {
        return;
    };
    for entry in entries.flatten() {
        if *count >= SCAN_ENTRIES {
            break;
        }
        *count += 1;
        let candidate = path.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            scan_markdown(root, &candidate, depth + 1, count, files);
        } else if kind.is_file() && candidate.extension().is_some_and(|e| e == "md") {
            files.push(candidate);
        }
    }
}
