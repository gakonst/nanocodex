//! Snapshot adapter for async hosts. The host gathers bounded, authorized bytes,
//! then applies the returned mutations after comparing their before-images.
use crate::portable_files::{FileEngine, FileHost, MAX_FILE, MAX_SEARCH_BYTES, MAX_VISITS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub root: String,
    pub name: String,
    pub input: Value,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default)]
    pub visits: usize,
    /// Discover filtered Grep read candidates using metadata only.
    #[serde(default)]
    pub prepare: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub content: Option<String>,
    pub size: Option<u64>,
    pub modified: Option<u64>,
}
#[derive(Serialize)]
pub struct Mutation {
    pub path: String,
    pub content: String,
    pub before: Option<String>,
}
#[derive(Serialize)]
pub struct Plan {
    pub output: String,
    pub mutations: Vec<Mutation>,
    pub reads: Vec<String>,
}
struct Snapshot {
    root: PathBuf,
    files: BTreeMap<PathBuf, File>,
    directories: BTreeSet<PathBuf>,
    mutations: RefCell<Vec<Mutation>>,
    reads: RefCell<BTreeSet<String>>,
    prepare: bool,
}
impl Snapshot {
    fn shown(&self, path: &Path) -> Result<String, String> {
        path.strip_prefix(&self.root)
            .map_err(|_| "path outside workspace".to_string())?
            .to_str()
            .map(str::to_owned)
            .ok_or("path is not UTF-8".into())
    }
}
impl FileHost for Snapshot {
    fn root(&self) -> &Path {
        &self.root
    }
    fn relative(&self, text: &str, allow_root: bool) -> Result<PathBuf, String> {
        let rel = crate::portable_files::relative_path(&self.root, &self.root, text, allow_root)?;
        if allow_root && rel == Path::new(".") {
            return Ok(PathBuf::new());
        }
        Ok(rel)
    }
    fn existing(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let rel = self.relative(text, true)?;
        let path = self.root.join(&rel);
        if !self.files.contains_key(&path) && !self.directories.contains(&path) {
            return Err("path does not exist".into());
        }
        Ok((rel, path))
    }
    fn file(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let pair = self.existing(text)?;
        if !self.files.contains_key(&pair.1) {
            return Err("path is not a regular file".into());
        }
        Ok(pair)
    }
    fn read_text(&self, path: &Path) -> Result<String, String> {
        self.reads.borrow_mut().insert(self.shown(path)?);
        if self.prepare {
            return Err("prepare phase has no bytes".into());
        }
        let file = self.files.get(path).ok_or("file does not exist")?;
        let content = file
            .content
            .as_ref()
            .ok_or("file is not UTF-8 text or exceeds 1 MiB limit")?;
        if content.len() > MAX_FILE {
            return Err("file exceeds 1 MiB text limit".into());
        }
        Ok(content.clone())
    }
    fn write_target(&self, text: &str) -> Result<(PathBuf, PathBuf), String> {
        let rel = self.relative(text, false)?;
        let path = self.root.join(&rel);
        if self.directories.contains(&path) {
            return Err("target is not a regular file".into());
        }
        for parent in path.ancestors().skip(1) {
            if self.files.contains_key(parent) {
                return Err("parent is not a directory".into());
            }
        }
        Ok((rel, path))
    }
    fn write(&self, path: &Path, content: &str) -> Result<(), String> {
        self.mutations.borrow_mut().push(Mutation {
            path: self.shown(path)?,
            content: content.into(),
            before: self.files.get(path).and_then(|f| f.content.clone()),
        });
        Ok(())
    }
    fn walk(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        Ok(self
            .files
            .keys()
            .filter(|p| p.starts_with(path))
            .cloned()
            .collect())
    }
    fn size(&self, path: &Path) -> Result<u64, String> {
        let file = self.files.get(path).ok_or("file does not exist")?;
        Ok(file
            .size
            .unwrap_or_else(|| file.content.as_ref().map_or(0, |c| c.len() as u64)))
    }
    fn modified(&self, path: &Path) -> Option<u128> {
        self.files
            .get(path)?
            .modified
            .map(|m| u128::from(m) * 1_000_000)
    }
    fn check_glob_prefix(&self, _root: &Path, _prefix: &str) -> Result<(), String> {
        Ok(())
    }
}

pub fn schemas() -> Vec<Value> {
    let mut result = FileEngine::<Snapshot>::definitions();
    result.extend(crate::portable_notebook::definitions());
    result
}

pub fn plan(request: Request) -> Result<Plan, String> {
    if request
        .files
        .len()
        .saturating_add(request.directories.len())
        .max(request.visits)
        > MAX_VISITS
    {
        return Err("search exceeds 10000 entries".into());
    }
    if request.root.is_empty()
        || !Path::new(&request.root).is_absolute()
        || Path::new(&request.root)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("root must be an absolute authorized workspace path".into());
    }
    if request.prepare && request.name != "Grep" {
        return Err("prepare is only supported for Grep".into());
    }
    let mut host = Snapshot {
        root: PathBuf::from(request.root),
        files: BTreeMap::new(),
        directories: BTreeSet::new(),
        mutations: RefCell::new(Vec::new()),
        reads: RefCell::new(BTreeSet::new()),
        prepare: request.prepare,
    };
    host.directories.insert(host.root.clone());
    for directory in request.directories {
        let path = host.root.join(host.relative(&directory, true)?);
        host.directories.insert(path);
    }
    let mut total = 0u64;
    for file in request.files {
        if let Some(content) = &file.content {
            if content.len() > MAX_FILE {
                return Err("file exceeds 1 MiB text limit".into());
            }
            if file.size.is_some_and(|s| s != content.len() as u64) {
                return Err("file size does not match content bytes".into());
            }
            total = total.saturating_add(content.len() as u64);
            if total > MAX_SEARCH_BYTES {
                return Err("search exceeds 128 MiB scan limit".into());
            }
        }
        let path = host.root.join(host.relative(&file.path, false)?);
        for parent in path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&host.root))
        {
            host.directories.insert(parent.to_path_buf());
        }
        if host.files.insert(path, file).is_some() {
            return Err("duplicate snapshot file".into());
        }
    }
    if host.files.keys().any(|p| host.directories.contains(p)) {
        return Err("snapshot path is both file and directory".into());
    }
    if host.files.len().saturating_add(host.directories.len()) > MAX_VISITS {
        return Err("search exceeds 10000 entries".into());
    }
    let output = if request.name == "NotebookEdit" {
        let raw = request
            .input
            .get("notebook_path")
            .and_then(Value::as_str)
            .ok_or("missing or invalid notebook_path")?;
        let (_, path) = host.file(raw)?;
        let before = host.read_text(&path)?;
        let (bytes, mut result) =
            crate::portable_notebook::edit(&request.input, before.as_bytes())?;
        result["notebook_path"] = Value::String(path.display().to_string());
        host.write(
            &path,
            std::str::from_utf8(&bytes).map_err(|_| "notebook is not UTF-8")?,
        )?;
        result.to_string()
    } else {
        if request.name == "Read" {
            let raw = request
                .input
                .get("file_path")
                .and_then(Value::as_str)
                .ok_or("missing or invalid file_path")?;
            host.file(raw)?;
            let ext = Path::new(raw)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf"
            ) {
                return Err(
                    "Read media requires a host media capability; text fallback is forbidden"
                        .into(),
                );
            }
            if ext == "ipynb" {
                return notebook_read_plan(&host, &request.input, raw);
            }
        }
        if request.name == "Read" {
            let raw = request.input["file_path"]
                .as_str()
                .ok_or("missing or invalid file_path")?;
            let (_, path) = host.file(raw)?;
            let content = host.read_text(&path)?;
            if content.starts_with("%PDF-")
                || content.starts_with("GIF87a")
                || content.starts_with("GIF89a")
                || (content.starts_with("RIFF") && content.as_bytes().get(8..12) == Some(b"WEBP"))
            {
                return Err(
                    "Read media requires a host media capability; text fallback is forbidden"
                        .into(),
                );
            }
        }
        FileEngine(&host).execute(&request.name, &request.input)?
    };
    Ok(Plan {
        output,
        mutations: host.mutations.into_inner(),
        reads: host.reads.into_inner().into_iter().collect(),
    })
}
fn notebook_read_plan(host: &Snapshot, input: &Value, raw: &str) -> Result<Plan, String> {
    if input.get("pages").is_some() {
        return Err("pages is only applicable to PDF files".into());
    }
    if input
        .as_object()
        .ok_or("Read input must be an object")?
        .keys()
        .any(|k| !["file_path", "offset", "limit", "pages"].contains(&k.as_str()))
    {
        return Err("unsupported Read option".into());
    }
    let (_, path) = host.file(raw)?;
    let bytes = host.read_text(&path)?;
    let output = crate::portable_notebook::read_text(bytes.as_bytes(), input)?;
    Ok(Plan {
        output,
        mutations: Vec::new(),
        reads: vec![host.shown(&path)?],
    })
}
