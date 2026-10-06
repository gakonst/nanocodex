//! Host-independent Claude file semantics. Hosts supply authorized, bounded IO.
use regex::RegexBuilder;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
pub const MAX_FILE: usize = 1024 * 1024;
pub const MAX_OUTPUT: usize = 64 * 1024;
pub const MAX_VISITS: usize = 10_000;
pub const MAX_SEARCH_BYTES: u64 = 128 * 1024 * 1024;

/// IO boundary: implementations must authorize paths and enforce isolation.
pub trait FileHost {
    fn root(&self) -> &Path;
    fn relative(&self, text: &str, allow_root: bool) -> Result<PathBuf, String>;
    fn existing(&self, text: &str) -> Result<(PathBuf, PathBuf), String>;
    fn file(&self, text: &str) -> Result<(PathBuf, PathBuf), String>;
    fn read_text(&self, path: &Path) -> Result<String, String>;
    fn write_target(&self, text: &str) -> Result<(PathBuf, PathBuf), String>;
    fn write(&self, path: &Path, content: &str) -> Result<(), String>;
    fn walk(&self, path: &Path) -> Result<Vec<PathBuf>, String>;
    fn size(&self, path: &Path) -> Result<u64, String>;
    fn modified(&self, path: &Path) -> Option<u128>;
    fn check_glob_prefix(&self, root: &Path, prefix: &str) -> Result<(), String>;
}
pub struct FileEngine<'a, H: FileHost>(pub &'a H);
impl<H: FileHost> FileEngine<'_, H> {
    pub fn definitions() -> Vec<Value> {
        vec![
            json!({"name":"Read","description":"Read text with numbered lines, images, PDF pages, or notebook cells and outputs. PDF reads require Poppler pdfinfo/pdftoppm; pages selects at most 20 pages and is required for PDFs over 10 pages.","input_schema":{"type":"object","properties":{"file_path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1},"pages":{"type":"string","description":"PDF page or inclusive range, e.g. 3 or 1-5; maximum 20 pages."}},"required":["file_path"],"additionalProperties":false}}),
            json!({"name":"Edit","description":"Replace exact text in a workspace file, requiring one occurrence unless replace_all is true.","input_schema":{"type":"object","properties":{"file_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["file_path","old_string","new_string"],"additionalProperties":false}}),
            json!({"name":"Write","description":"Atomically replace a UTF-8 workspace file, creating parent directories as needed.","input_schema":{"type":"object","properties":{"file_path":{"type":"string"},"content":{"type":"string"}},"required":["file_path","content"],"additionalProperties":false}}),
            json!({"name":"Glob","description":"List workspace files using glob wildcards, braces and character classes, newest first.","input_schema":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"],"additionalProperties":false}}),
            json!({"name":"Grep","description":"Search text using a bounded Rust regex, ripgrep file types and glob syntax. Default output is matching file paths.","input_schema":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string","description":"File glob, including braces and character classes. Prefix ! to exclude matches."},"type":{"type":"string","description":"Ripgrep file type, for example rust, py, js, ts or all."},"output_mode":{"type":"string","enum":["content","files_with_matches","count"],"default":"files_with_matches"},"-B":{"type":"integer","minimum":0,"maximum":2000},"-A":{"type":"integer","minimum":0,"maximum":2000},"-C":{"type":"integer","minimum":0,"maximum":2000},"context":{"type":"integer","minimum":0,"maximum":2000},"-n":{"type":"boolean","default":true},"-i":{"type":"boolean"},"-o":{"type":"boolean"},"head_limit":{"type":"integer","minimum":0,"default":250},"offset":{"type":"integer","minimum":0,"default":0},"multiline":{"type":"boolean"}},"required":["pattern"],"additionalProperties":false}}),
        ]
    }
    pub fn execute(&self, name: &str, input: &Value) -> Result<String, String> {
        let allowed: &[&str] = match name {
            "Read" => &["file_path", "offset", "limit", "pages"],
            "Write" => &["file_path", "content"],
            "Edit" => &["file_path", "old_string", "new_string", "replace_all"],
            "Glob" => &["pattern", "path"],
            // Grep validates its larger option set below.
            "Grep" => return self.grep(input),
            _ => return Err(format!("unknown workspace tool: {name}")),
        };
        let fields = input
            .as_object()
            .ok_or("workspace input must be an object")?;
        if let Some(key) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(format!("unsupported {name} option: {key}"));
        }
        match name {
            "Read" => self.read(input),
            "Write" => self.write(input),
            "Edit" => self.edit(input),
            "Glob" => self.glob(input),
            "Grep" => self.grep(input),
            _ => Err(format!("unknown workspace tool: {name}")),
        }
    }
    fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing or invalid {key}"))
    }
    fn read(&self, input: &Value) -> Result<String, String> {
        if input.get("pages").is_some() {
            return Err("pages is only applicable to PDF files".into());
        }
        let (_, path) = self.0.file(Self::field(input, "file_path")?)?;
        let content = self.0.read_text(&path)?;
        let offset = input
            .get("offset")
            .map_or(Some(1), Value::as_u64)
            .ok_or("invalid offset")?;
        let limit = input
            .get("limit")
            .map_or(Some(2000), Value::as_u64)
            .ok_or("invalid limit")?;
        if offset == 0 || limit == 0 {
            return Err("offset and limit must be positive".into());
        }
        let mut out = String::new();
        for (i, line) in content
            .lines()
            .enumerate()
            .skip(offset.saturating_sub(1).min(usize::MAX as u64) as usize)
            .take(limit.min(2000) as usize)
        {
            if !push_bounded(&mut out, &format!("{}\t{}\n", i + 1, line)) {
                break;
            }
        }
        Ok(out)
    }
    fn write(&self, input: &Value) -> Result<String, String> {
        let content = Self::field(input, "content")?;
        if content.len() > MAX_FILE {
            return Err("content exceeds 1 MiB text limit".into());
        }
        let (rel, path) = self.0.write_target(Self::field(input, "file_path")?)?;
        self.0.write(&path, content)?;
        Ok(format!("Wrote {}", rel.display()))
    }
    fn edit(&self, input: &Value) -> Result<String, String> {
        let text = Self::field(input, "file_path")?;
        let old = Self::field(input, "old_string")?;
        let new = Self::field(input, "new_string")?;
        if old.is_empty() {
            return Err("old_string must not be empty".into());
        }
        let replace_all = input
            .get("replace_all")
            .map_or(Some(false), Value::as_bool)
            .ok_or("invalid replace_all")?;
        let (rel, existing) = self.0.file(text)?;
        let content = self.0.read_text(&existing)?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err("old_string not found".into());
        }
        if count != 1 && !replace_all {
            return Err(format!("old_string occurs {count} times; set replace_all"));
        }
        // Validate expansion before allocating: a tiny repeated match can otherwise
        // amplify a bounded input into an unbounded replacement allocation.
        let replacements = if replace_all { count } else { 1 };
        let updated_len = content.len() - old.len() * replacements;
        let updated_len = new
            .len()
            .checked_mul(replacements)
            .and_then(|added| updated_len.checked_add(added))
            .filter(|&len| len <= MAX_FILE)
            .ok_or("edited content exceeds 1 MiB text limit")?;
        let updated = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        debug_assert_eq!(updated.len(), updated_len);
        let (_, path) = self.0.write_target(text)?;
        // Best-effort stale-read check before the atomic replacement.
        if self.0.read_text(&path)? != content {
            return Err("file changed during edit".into());
        }
        self.0.write(&path, &updated)?;
        Ok(format!("Edited {} ({count} replacement(s))", rel.display()))
    }
    fn search_root(&self, input: &Value) -> Result<(PathBuf, PathBuf), String> {
        let text = input
            .get("path")
            .map_or(Some("."), Value::as_str)
            .ok_or("invalid path")?;
        self.0.existing(text)
    }
    fn glob(&self, input: &Value) -> Result<String, String> {
        let raw_pattern = Self::field(input, "pattern")?;
        let pattern = if Path::new(raw_pattern).is_absolute() {
            self.0
                .relative(raw_pattern, false)?
                .to_str()
                .ok_or("pattern is not UTF-8")?
                .to_owned()
        } else {
            raw_pattern.to_owned()
        };
        let matcher = glob_regex(&pattern)?;
        // An explicit literal prefix denoting a symlink escape is an error, not an empty match.
        let literal_prefix = pattern
            .split('/')
            .take_while(|part| !part.contains(['*', '?', '[', '{', '\\']))
            .collect::<Vec<_>>()
            .join("/");
        let (_, root) = self.search_root(input)?;
        self.0.check_glob_prefix(&root, &literal_prefix)?;
        let mut out = String::new();
        let mut files = self.0.walk(&root)?;
        files.sort_by_cached_key(|path| (std::cmp::Reverse(self.0.modified(path)), path.clone()));
        for file in files {
            let rel = file
                .strip_prefix(&root)
                .map_err(|_| "search path changed")?
                .to_string_lossy();
            if matcher.is_match(rel.as_ref()) {
                let shown = file
                    .strip_prefix(self.0.root())
                    .map_err(|_| "search escaped workspace")?
                    .display();
                if !push_bounded(&mut out, &format!("{shown}\n")) {
                    break;
                }
            }
        }
        Ok(out)
    }
    fn grep(&self, input: &Value) -> Result<String, String> {
        let fields = input.as_object().ok_or("Grep input must be an object")?;
        for key in fields.keys() {
            if !matches!(
                key.as_str(),
                "pattern"
                    | "path"
                    | "glob"
                    | "type"
                    | "output_mode"
                    | "-B"
                    | "-A"
                    | "-C"
                    | "context"
                    | "-n"
                    | "-i"
                    | "-o"
                    | "head_limit"
                    | "offset"
                    | "multiline"
                    | "case_sensitive"
            ) {
                return Err(format!("unsupported Grep option: {key}"));
            }
        }
        let pattern = Self::field(input, "pattern")?;
        if pattern.is_empty() || pattern.len() > 4096 {
            return Err("pattern must be 1 to 4096 bytes".into());
        }
        let mode = input
            .get("output_mode")
            .map_or(Some("files_with_matches"), Value::as_str)
            .ok_or("invalid output_mode")?;
        if !matches!(mode, "content" | "files_with_matches" | "count") {
            return Err("invalid output_mode".into());
        }
        let insensitive = grep_bool(input, "-i", false)?;
        if input.get("case_sensitive").is_some() && input.get("-i").is_some() {
            return Err("case_sensitive conflicts with -i".into());
        }
        // Retain the original, unadvertised spelling for old callers.
        let sensitive = grep_bool(input, "case_sensitive", !insensitive)?;
        let only_matching = grep_bool(input, "-o", false)?;
        let line_numbers = grep_bool(input, "-n", true)?;
        let multiline = grep_bool(input, "multiline", false)?;
        let before = grep_number(input, "-B", 0, 2000)?;
        let after = grep_number(input, "-A", 0, 2000)?;
        if input.get("-C").is_some() && input.get("context").is_some() {
            return Err("-C conflicts with context".into());
        }
        let context = if input.get("-C").is_some() {
            grep_number(input, "-C", 0, 2000)?
        } else {
            grep_number(input, "context", 0, 2000)?
        };
        let before = if input.get("-B").is_some() {
            before
        } else {
            context
        };
        let after = if input.get("-A").is_some() {
            after
        } else {
            context
        };
        let offset = grep_number(input, "offset", 0, u64::MAX)?;
        let head_limit = grep_number(input, "head_limit", 250, u64::MAX)?;
        if only_matching && (before > 0 || after > 0 || multiline) {
            return Err("-o cannot be combined with context or multiline".into());
        }
        let glob = match input.get("glob") {
            Some(value) => Some(value.as_str().ok_or("invalid glob")?),
            None => None,
        };
        let glob_exclude = glob.is_some_and(|g| g.starts_with('!'));
        let glob = glob.map(|g| g.strip_prefix('!').unwrap_or(g));
        let glob_matcher = glob.map(glob_regex).transpose()?;
        let mut types = ignore::types::TypesBuilder::new();
        types.add_defaults();
        if let Some(kind) = input.get("type") {
            let kind = kind.as_str().ok_or("invalid type")?;
            if kind.is_empty() || kind.len() > 64 {
                return Err("invalid type".into());
            }
            types.select(kind);
        }
        let types = types
            .build()
            .map_err(|e| format!("invalid file type: {e}"))?;
        let re = RegexBuilder::new(pattern)
            .case_insensitive(!sensitive)
            .multi_line(multiline)
            .dot_matches_new_line(multiline)
            .size_limit(4 * 1024 * 1024)
            .build()
            .map_err(|e| format!("invalid or oversized regex: {e}"))?;
        let (_, root) = self.search_root(input)?;
        let mut out = String::new();
        let mut searched_bytes = 0u64;
        let mut skipped = 0u64;
        let mut yielded = 0u64;
        for file in self.0.walk(&root)? {
            let shown = file
                .strip_prefix(self.0.root())
                .map_err(|_| "search escaped workspace")?
                .to_string_lossy();
            if let (Some(glob), Some(matcher)) = (glob, &glob_matcher) {
                let relative = file
                    .strip_prefix(&root)
                    .map_err(|_| "search path changed")?;
                let target = if glob.contains('/') && !relative.as_os_str().is_empty() {
                    relative.to_string_lossy()
                } else {
                    file.file_name().unwrap_or_default().to_string_lossy()
                };
                if matcher.is_match(target.as_ref()) == glob_exclude {
                    continue;
                }
            }
            if types.matched(&file, false).is_ignore() {
                continue;
            }
            let size = self.0.size(&file)?;
            searched_bytes = searched_bytes.saturating_add(size.min((MAX_FILE + 1) as u64));
            if searched_bytes > MAX_SEARCH_BYTES {
                return Err("search exceeds 128 MiB scan limit".into());
            }
            let Ok(contents) = self.0.read_text(&file) else {
                continue;
            };
            // Like ripgrep, do not treat NUL-containing binary data as text.
            if contents.contains('\0') {
                continue;
            }
            let lines: Vec<&str> = contents.lines().collect();
            let mut hits = vec![false; lines.len()];
            if multiline {
                // A match spanning lines marks each touched line. Byte offsets are UTF-8 safe.
                let starts: Vec<usize> = std::iter::once(0)
                    .chain(contents.match_indices('\n').map(|(i, _)| i + 1))
                    .collect();
                for found in re.find_iter(&contents) {
                    let first = starts
                        .partition_point(|&s| s <= found.start())
                        .saturating_sub(1);
                    let last_byte = found.end().saturating_sub(1).max(found.start());
                    let last = starts
                        .partition_point(|&s| s <= last_byte)
                        .saturating_sub(1);
                    for hit in hits.iter_mut().take(last.saturating_add(1)).skip(first) {
                        *hit = true;
                    }
                }
            } else {
                for (line, hit) in lines.iter().zip(&mut hits) {
                    *hit = re.is_match(line);
                }
            }
            if mode != "content" {
                let count = hits.iter().filter(|&&hit| hit).count();
                if count == 0 {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if head_limit != 0 && yielded >= head_limit {
                    return Ok(out);
                }
                let line = if mode == "count" {
                    format!("{shown}:{count}\n")
                } else {
                    format!("{shown}\n")
                };
                if !push_bounded(&mut out, &line) {
                    return Ok(out);
                }
                yielded += 1;
                continue;
            }
            if only_matching {
                for (index, line) in lines.iter().enumerate() {
                    for found in re.find_iter(line) {
                        if skipped < offset {
                            skipped += 1;
                            continue;
                        }
                        if head_limit != 0 && yielded >= head_limit {
                            return Ok(out);
                        }
                        let prefix = if line_numbers {
                            format!("{shown}:{}:", index + 1)
                        } else {
                            format!("{shown}:")
                        };
                        if !push_bounded(&mut out, &format!("{prefix}{}\n", found.as_str())) {
                            return Ok(out);
                        }
                        yielded += 1;
                    }
                }
                continue;
            }
            let mut selected = vec![false; lines.len()];
            for (index, hit) in hits.iter().enumerate() {
                if !hit {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if head_limit != 0 && yielded >= head_limit {
                    break;
                }
                selected[index] = true;
                yielded += 1;
            }
            let mut emit = vec![false; lines.len()];
            for (index, &hit) in selected.iter().enumerate() {
                if hit {
                    let start = index.saturating_sub(before as usize);
                    let end = index
                        .saturating_add(after as usize)
                        .saturating_add(1)
                        .min(lines.len());
                    emit[start..end].fill(true);
                }
            }
            let mut previous = None;
            for (index, line) in lines.iter().enumerate() {
                if !emit[index] {
                    continue;
                }
                if let Some(prev) = previous
                    && index > prev + 1
                    && !push_bounded(&mut out, "--\n")
                {
                    return Ok(out);
                }
                let separator = if selected[index] { ':' } else { '-' };
                let result = if line_numbers {
                    format!("{shown}{separator}{}{separator}{line}\n", index + 1)
                } else {
                    format!("{shown}{separator}{line}\n")
                };
                if !push_bounded(&mut out, &result) {
                    return Ok(out);
                }
                previous = Some(index);
            }
            if head_limit != 0 && yielded >= head_limit {
                return Ok(out);
            }
        }
        Ok(out)
    }
}
fn grep_bool(input: &Value, key: &str, default: bool) -> Result<bool, String> {
    input
        .get(key)
        .map_or(Some(default), Value::as_bool)
        .ok_or_else(|| format!("invalid {key}"))
}

fn grep_number(input: &Value, key: &str, default: u64, max: u64) -> Result<u64, String> {
    let value = input
        .get(key)
        .map_or(Some(default), Value::as_u64)
        .ok_or_else(|| format!("invalid {key}"))?;
    if value > max {
        return Err(format!("{key} exceeds {max}"));
    }
    Ok(value)
}

fn push_bounded(output: &mut String, line: &str) -> bool {
    if output.len() + line.len() > MAX_OUTPUT {
        const MARKER: &str = "[output truncated]\n";
        if output.len() + MARKER.len() <= MAX_OUTPUT {
            output.push_str(MARKER);
        }
        return false;
    }
    output.push_str(line);
    true
}

fn validate_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty()
        || pattern.len() > 512
        || pattern.starts_with('/')
        || pattern.split('/').any(|part| part == ".." || part == ".")
    {
        return Err("invalid glob pattern or traversal".into());
    }
    Ok(())
}

// The same glob engine used by ripgrep supports braces, classes, escapes and **.
fn glob_regex(pattern: &str) -> Result<globset::GlobMatcher, String> {
    validate_pattern(pattern)?;
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| format!("invalid glob: {e}"))
}

/// Shared lexical validation. Filesystem hosts additionally resolve and authorize symlinks.
pub fn relative_path(
    root: &Path,
    root_alias: &Path,
    text: &str,
    allow_root: bool,
) -> Result<PathBuf, String> {
    if text.contains('\0') {
        return Err("path contains NUL".into());
    }
    if text.len() > 4096 {
        return Err("path exceeds 4096-byte limit".into());
    }
    let path = Path::new(text);
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .or_else(|_| path.strip_prefix(root_alias))
            .map_err(|_| "absolute path outside workspace".to_string())?
    } else {
        path
    };
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        // A bare "." is allowed only as a search root.
        if !(allow_root && (text == "." || relative.as_os_str().is_empty())) {
            return Err("path must not contain traversal, root, or special components".into());
        }
    }
    if !allow_root && relative.as_os_str().is_empty() {
        return Err("file path is empty".into());
    }
    Ok(relative.to_path_buf())
}
