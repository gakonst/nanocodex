use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use nanocodex_core::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::runtime::{Tool, ToolContext, ToolExecution, ToolInput, ToolResult};

mod block;
mod format;
mod hash;
mod patch;
mod patch_lines;
mod patch_parser;
mod patch_sections;

use block::{find_normalized_block_span, language_for_path, resolve_find_block_anchor};
use format::{build_hashline_excerpt, split_lines_preserve};
use hash::{hash_hex, line_hash, normalize_file_text};
use patch::{
    HashlinePatchFileOperation, apply_hashline_patch, build_hashline_patch_preview,
    hashline_patch_has_line_operations, hashline_patch_is_aborted,
    parse_hashline_patch_file_operation, validate_file_hash,
};
use patch_sections::{
    HashlinePatchSection, split_hashline_patch_sections, split_hashline_patch_sections_for_create,
};

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_MUTATIONS: usize = 64;
const DEFAULT_READ_MAX_LINES: usize = 200;
const HARD_READ_MAX_LINES: usize = 1000;
const DEFAULT_BLOCK_MAX_LINES: usize = 80;
const HARD_BLOCK_MAX_LINES: usize = 300;
const READ_OUTPUT_BYTES: usize = 24 * 1024;
const BLOCK_OUTPUT_BYTES: usize = 24 * 1024;

#[derive(Debug)]
pub(super) enum FunctionCallError {
    RespondToModel(String),
}

impl fmt::Display for FunctionCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RespondToModel(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for FunctionCallError {}

pub(super) fn take_bytes_at_char_boundary(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[derive(Clone, Copy)]
pub(super) enum HashlineToolKind {
    Read,
    FindBlock,
    Patch,
    Transaction,
}

pub(super) struct HashlineHandler {
    workspace: PathBuf,
    kind: HashlineToolKind,
}

impl HashlineHandler {
    pub(super) fn new(workspace: PathBuf, kind: HashlineToolKind) -> Self {
        Self { workspace, kind }
    }
}

#[async_trait::async_trait]
impl Tool for HashlineHandler {
    fn name(&self) -> &'static str {
        match self.kind {
            HashlineToolKind::Read => "hashline__read",
            HashlineToolKind::FindBlock => "hashline__find_block",
            HashlineToolKind::Patch => "hashline__patch",
            HashlineToolKind::Transaction => "hashline__transaction",
        }
    }

    fn definition(&self) -> ToolDefinition {
        match self.kind {
            HashlineToolKind::Read => read_definition(),
            HashlineToolKind::FindBlock => block_definition(),
            HashlineToolKind::Patch => patch_definition(),
            HashlineToolKind::Transaction => transaction_definition(),
        }
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let raw = input.function_json()?.get().to_owned();
        let workspace = self.workspace.clone();
        let kind = self.kind;
        Ok(
            match tokio::task::spawn_blocking(move || execute(kind, &workspace, &raw)).await {
                Ok(Ok(value)) => ToolExecution::from_json(value, true),
                Ok(Err(error)) => ToolExecution::error(error.to_string()),
                Err(error) => ToolExecution::error(format!("Hashline task failed: {error}")),
            },
        )
    }
}

fn execute(
    kind: HashlineToolKind,
    workspace: &Path,
    raw: &str,
) -> Result<Value, FunctionCallError> {
    match kind {
        HashlineToolKind::Read => read(workspace, &decode(raw)?),
        HashlineToolKind::FindBlock => find_block(workspace, &decode(raw)?),
        HashlineToolKind::Patch => execute_patch(workspace, &decode(raw)?),
        HashlineToolKind::Transaction => execute_transaction(workspace, &decode(raw)?),
    }
}

fn decode<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, FunctionCallError> {
    serde_json::from_str(raw).map_err(|error| {
        FunctionCallError::RespondToModel(format!("invalid Hashline arguments: {error}"))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_lines: Option<usize>,
}

fn read(workspace: &Path, request: &ReadRequest) -> Result<Value, FunctionCallError> {
    let observed = observe(workspace, &request.path)?;
    let normalized = normalize_file_text(&observed.text);
    let lines = split_lines_preserve(&normalized);
    if lines.is_empty() {
        return Ok(json!({
            "path": request.path,
            "hash": hash_hex(&observed.text),
            "exactDigest": exact_digest(&observed.bytes),
            "header": format!("[{}]#{}", request.path, hash_hex(&observed.text)),
            "start_line": null,
            "end_line": null,
            "total_lines": 0,
            "truncated": false,
            "next_start_line": null,
            "content": "",
            "lines": [],
        }));
    }
    let start = request.start_line.unwrap_or(1);
    if start == 0 || start > lines.len() {
        return model_error(format!(
            "start_line {start} is outside file range 1..={}",
            lines.len()
        ));
    }
    let requested_end = request.end_line.unwrap_or(lines.len());
    if requested_end < start {
        return model_error("end_line must be greater than or equal to start_line");
    }
    let max_lines = request
        .max_lines
        .unwrap_or(DEFAULT_READ_MAX_LINES)
        .clamp(1, HARD_READ_MAX_LINES);
    let end = requested_end
        .min(lines.len())
        .min(start.saturating_add(max_lines).saturating_sub(1));
    let excerpt = build_hashline_excerpt(&lines, start, end, READ_OUTPUT_BYTES);
    let returned_end = excerpt.end_line;
    let truncated =
        excerpt.truncated || returned_end.is_some_and(|line| line < requested_end.min(lines.len()));
    let next_start = truncated.then(|| returned_end.map_or(start, |line| line + 1));
    let file_hash = hash_hex(&observed.text);
    Ok(json!({
        "path": request.path,
        "hash": file_hash,
        "exactDigest": exact_digest(&observed.bytes),
        "header": format!("[{}]#{file_hash}", request.path),
        "start_line": start,
        "end_line": returned_end,
        "total_lines": lines.len(),
        "truncated": truncated,
        "next_start_line": next_start,
        "content": excerpt.content,
        "lines": excerpt.lines,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockRequest {
    path: String,
    anchor: String,
    max_lines: Option<usize>,
}

fn find_block(workspace: &Path, request: &BlockRequest) -> Result<Value, FunctionCallError> {
    let observed = observe(workspace, &request.path)?;
    let normalized = normalize_file_text(&observed.text);
    let lines = split_lines_preserve(&normalized);
    if lines.is_empty() {
        return model_error("cannot find a block in an empty file");
    }
    let anchor_line = resolve_find_block_anchor(&request.path, &request.anchor, &lines)?;
    let (start, end) = find_normalized_block_span(&request.path, &lines, anchor_line);
    let cap = request
        .max_lines
        .unwrap_or(DEFAULT_BLOCK_MAX_LINES)
        .clamp(1, HARD_BLOCK_MAX_LINES);
    let excerpt_end = end.min(start.saturating_add(cap).saturating_sub(1));
    let excerpt = build_hashline_excerpt(&lines, start, excerpt_end, BLOCK_OUTPUT_BYTES);
    let file_hash = hash_hex(&observed.text);
    let block_hash = hash_hex(&lines[start - 1..end].join("\n"));
    let anchor_hash = line_hash(lines[anchor_line - 1]);
    Ok(json!({
        "path": request.path,
        "hash": file_hash,
        "exactDigest": exact_digest(&observed.bytes),
        "header": format!("[{}]#{file_hash}", request.path),
        "block_hash": block_hash,
        "block_anchor": format!("{anchor_line}:{anchor_hash}@{block_hash}"),
        "anchor": request.anchor,
        "line_count": lines.len(),
        "language": language_for_path(&request.path),
        "start_line": start,
        "end_line": end,
        "truncated": excerpt_end < end || excerpt.truncated,
        "content": excerpt.content,
        "lines": excerpt.lines,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRequest {
    path: String,
    patch: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    create: bool,
}

#[derive(Clone)]
enum PreparedMutation {
    Write {
        path: String,
        before: Option<Vec<u8>>,
        after: Vec<u8>,
    },
    Delete {
        path: String,
        before: Vec<u8>,
    },
    Move {
        source: String,
        destination: String,
        before: Vec<u8>,
        after: Vec<u8>,
    },
}

fn execute_patch(workspace: &Path, request: &PatchRequest) -> Result<Value, FunctionCallError> {
    if hashline_patch_is_aborted(&request.patch) {
        return Ok(
            json!({"success": true, "operation": "abort", "aborted": true, "dry_run": request.dry_run}),
        );
    }
    let sections = if request.create {
        split_hashline_patch_sections_for_create(&request.path, &request.patch)?
    } else {
        split_hashline_patch_sections(&request.path, &request.patch)?
    };
    if sections.len() > MAX_MUTATIONS {
        return model_error(format!(
            "Hashline patch exceeds the {MAX_MUTATIONS}-file limit"
        ));
    }
    let mut prepared = Vec::with_capacity(sections.len());
    let mut details = Vec::with_capacity(sections.len());
    for section in sections {
        let mutation = prepare_patch_section(workspace, &section, request.create)?;
        details.push(preview_mutation(&mutation)?);
        prepared.push(mutation);
    }
    reject_conflicts(&prepared)?;
    if !request.dry_run {
        apply_prepared(workspace, &prepared)?;
    }
    let total_files = details.len();
    let mut bounded_details = Vec::new();
    let mut detail_bytes = 2_usize;
    for detail in details {
        let bytes = serde_json::to_vec(&detail)
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        if detail_bytes.saturating_add(bytes.len()).saturating_add(1) > READ_OUTPUT_BYTES {
            break;
        }
        detail_bytes = detail_bytes.saturating_add(bytes.len()).saturating_add(1);
        bounded_details.push(detail);
    }
    let files_truncated = bounded_details.len() < total_files;
    Ok(json!({
        "success": true,
        "dry_run": request.dry_run,
        "recoverability": "validated before the first write; live failures use best-effort rollback; process death may leave mixed state",
        "total_files": total_files,
        "files_truncated": files_truncated,
        "files": bounded_details,
    }))
}

fn prepare_patch_section(
    workspace: &Path,
    section: &HashlinePatchSection,
    create: bool,
) -> Result<PreparedMutation, FunctionCallError> {
    validate_model_path(&section.path)?;
    if create {
        ensure_missing(workspace, &section.path)?;
        if section.expected_hash.is_some() {
            return model_error("create=true sections cannot include file hashes");
        }
        let after = if section.patch.trim().is_empty() {
            Vec::new()
        } else {
            apply_hashline_patch(&section.path, "", &section.patch)?.into_bytes()
        };
        return Ok(PreparedMutation::Write {
            path: section.path.clone(),
            before: None,
            after,
        });
    }
    let observed = observe(workspace, &section.path)?;
    let expected = section.expected_hash.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "existing-file Hashline patches require a [{}]#HASH section header",
            section.path
        ))
    })?;
    validate_file_hash(&section.path, &observed.text, expected)?;
    match parse_hashline_patch_file_operation(&section.patch)? {
        Some(HashlinePatchFileOperation::Remove) => Ok(PreparedMutation::Delete {
            path: section.path.clone(),
            before: observed.bytes,
        }),
        Some(HashlinePatchFileOperation::Rename { new_path }) => {
            validate_model_path(&new_path)?;
            ensure_missing(workspace, &new_path)?;
            let after = if hashline_patch_has_line_operations(&section.patch)? {
                apply_hashline_patch(&section.path, &observed.text, &section.patch)?.into_bytes()
            } else {
                observed.bytes.clone()
            };
            Ok(PreparedMutation::Move {
                source: section.path.clone(),
                destination: new_path,
                before: observed.bytes,
                after,
            })
        }
        None => {
            let after =
                apply_hashline_patch(&section.path, &observed.text, &section.patch)?.into_bytes();
            Ok(PreparedMutation::Write {
                path: section.path.clone(),
                before: Some(observed.bytes),
                after,
            })
        }
    }
}

fn preview_mutation(mutation: &PreparedMutation) -> Result<Value, FunctionCallError> {
    match mutation {
        PreparedMutation::Write {
            path,
            before,
            after,
        } => {
            let old = before
                .as_deref()
                .map(str_from_bytes)
                .transpose()?
                .unwrap_or("");
            let new = str_from_bytes(after)?;
            let preview = build_hashline_patch_preview(old, new)?;
            Ok(json!({
                "path": path,
                "operation": if before.is_some() { "update" } else { "create" },
                "old_exact_digest": before.as_deref().map(exact_digest),
                "new_exact_digest": exact_digest(after),
                "new_hash": hash_hex(new),
                "preview": preview,
            }))
        }
        PreparedMutation::Delete { path, before } => Ok(json!({
            "path": path,
            "operation": "delete",
            "old_exact_digest": exact_digest(before),
        })),
        PreparedMutation::Move {
            source,
            destination,
            before,
            after,
        } => Ok(json!({
            "path": source,
            "new_path": destination,
            "operation": "move",
            "old_exact_digest": exact_digest(before),
            "new_exact_digest": exact_digest(after),
            "new_hash": hash_hex(str_from_bytes(after)?),
        })),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
enum TransactionAction {
    Preview,
    Commit,
    CommitPreviewed {
        #[serde(rename = "expectedPlanDigest", alias = "expected_plan_digest")]
        expected_plan_digest: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedFile {
    exact_digest: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LineAnchor {
    line: usize,
    expected_hash: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LineRange {
    start: LineAnchor,
    end: LineAnchor,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
enum FileEdit {
    ReplaceAll {
        contents: String,
    },
    ReplaceLines {
        range: LineRange,
        lines: Vec<String>,
    },
    InsertBefore {
        anchor: LineAnchor,
        lines: Vec<String>,
    },
    InsertAfter {
        anchor: LineAnchor,
        lines: Vec<String>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
enum FileMutation {
    Create {
        path: String,
        contents: String,
    },
    Update {
        path: String,
        expected: ExpectedFile,
        edits: Vec<FileEdit>,
    },
    Delete {
        path: String,
        expected: ExpectedFile,
    },
    Move {
        source: String,
        expected: ExpectedFile,
        destination: String,
        edits: Vec<FileEdit>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransactionRequest {
    action: TransactionAction,
    #[serde(default)]
    root: Option<String>,
    mutations: Vec<FileMutation>,
}

fn execute_transaction(
    workspace: &Path,
    request: &TransactionRequest,
) -> Result<Value, FunctionCallError> {
    if request.mutations.is_empty() || request.mutations.len() > MAX_MUTATIONS {
        return model_error(format!(
            "Hashline transaction requires 1..={MAX_MUTATIONS} mutations"
        ));
    }
    let root_name = request.root.as_deref().unwrap_or(".");
    let root = resolve_root(workspace, root_name)?;
    ensure_transaction_capability(&root)?;
    let _lease = acquire_transaction_lease(&root)?;
    recover_pending(&root)?;
    let prepared = prepare_transaction(&root, &request.mutations)?;
    reject_conflicts(&prepared)?;
    let digest_input =
        serde_json::to_vec(&(root_name, &request.mutations, prepared_digests(&prepared)))
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    let plan_digest = exact_digest(&digest_input);
    if let TransactionAction::CommitPreviewed {
        expected_plan_digest,
    } = &request.action
        && expected_plan_digest != &plan_digest
    {
        return model_error(format!(
            "preview plan digest mismatch: expected {expected_plan_digest}, found {plan_digest}; preview the transaction again"
        ));
    }
    let previews = prepared
        .iter()
        .map(preview_mutation)
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(request.action, TransactionAction::Preview) {
        return Ok(json!({
            "outcome": "previewed",
            "planDigest": plan_digest,
            "mutations": previews,
            "preview_truncated": false,
        }));
    }
    let transaction_id = plan_digest.clone();
    write_journal(&root, &transaction_id, &prepared)?;
    match apply_prepared(&root, &prepared) {
        Ok(()) => {
            remove_journal(&root, &transaction_id)?;
            Ok(json!({
                "outcome": "committed",
                "transactionId": transaction_id,
                "planDigest": plan_digest,
                "mutations": previews,
            }))
        }
        Err(error) => Err(error),
    }
}

fn prepare_transaction(
    root: &Path,
    mutations: &[FileMutation],
) -> Result<Vec<PreparedMutation>, FunctionCallError> {
    let mut prepared = Vec::with_capacity(mutations.len());
    let mut total = 0_usize;
    for mutation in mutations {
        let item = match mutation {
            FileMutation::Create { path, contents } => {
                validate_model_path(path)?;
                ensure_missing(root, path)?;
                validate_transaction_parent(root, path)?;
                PreparedMutation::Write {
                    path: path.clone(),
                    before: None,
                    after: contents.as_bytes().to_vec(),
                }
            }
            FileMutation::Update {
                path,
                expected,
                edits,
            } => {
                let observed = observe(root, path)?;
                validate_exact(path, &observed.bytes, &expected.exact_digest)?;
                let after = apply_transaction_edits(path, &observed.text, edits)?;
                PreparedMutation::Write {
                    path: path.clone(),
                    before: Some(observed.bytes),
                    after,
                }
            }
            FileMutation::Delete { path, expected } => {
                let observed = observe(root, path)?;
                validate_exact(path, &observed.bytes, &expected.exact_digest)?;
                PreparedMutation::Delete {
                    path: path.clone(),
                    before: observed.bytes,
                }
            }
            FileMutation::Move {
                source,
                expected,
                destination,
                edits,
            } => {
                let observed = observe(root, source)?;
                validate_exact(source, &observed.bytes, &expected.exact_digest)?;
                validate_model_path(destination)?;
                ensure_missing(root, destination)?;
                validate_transaction_parent(root, destination)?;
                let after = if edits.is_empty() {
                    observed.bytes.clone()
                } else {
                    apply_transaction_edits(source, &observed.text, edits)?
                };
                PreparedMutation::Move {
                    source: source.clone(),
                    destination: destination.clone(),
                    before: observed.bytes,
                    after,
                }
            }
        };
        total = total.saturating_add(mutation_bytes(&item));
        if total > MAX_TOTAL_BYTES {
            return model_error(format!(
                "Hashline transaction exceeds the {MAX_TOTAL_BYTES}-byte limit"
            ));
        }
        prepared.push(item);
    }
    Ok(prepared)
}

fn apply_transaction_edits(
    path: &str,
    contents: &str,
    edits: &[FileEdit],
) -> Result<Vec<u8>, FunctionCallError> {
    if edits.is_empty() {
        return model_error(format!("transaction update for {path} has no edits"));
    }
    if let [FileEdit::ReplaceAll { contents }] = edits {
        if contents.as_bytes().contains(&0) {
            return model_error("transaction contents must be UTF-8 text without NUL bytes");
        }
        return Ok(contents.as_bytes().to_vec());
    }
    if edits
        .iter()
        .any(|edit| matches!(edit, FileEdit::ReplaceAll { .. }))
    {
        return model_error("replaceAll must be the only edit for a mutation");
    }
    let mut patch_text = format!("[{path}]#{}", hash_hex(contents));
    for edit in edits {
        match edit {
            FileEdit::ReplaceLines { range, lines } => {
                let _ = write!(
                    &mut patch_text,
                    "\nSWAP {}:{}..={}:{}:",
                    range.start.line,
                    range.start.expected_hash,
                    range.end.line,
                    range.end.expected_hash
                );
                append_payload(&mut patch_text, lines)?;
            }
            FileEdit::InsertBefore { anchor, lines } => {
                let _ = write!(
                    &mut patch_text,
                    "\nINS.PRE {}:{}:",
                    anchor.line, anchor.expected_hash
                );
                append_payload(&mut patch_text, lines)?;
            }
            FileEdit::InsertAfter { anchor, lines } => {
                let _ = write!(
                    &mut patch_text,
                    "\nINS.POST {}:{}:",
                    anchor.line, anchor.expected_hash
                );
                append_payload(&mut patch_text, lines)?;
            }
            FileEdit::ReplaceAll { .. } => unreachable!("replaceAll handled above"),
        }
    }
    Ok(apply_hashline_patch(path, contents, &patch_text)?.into_bytes())
}

fn append_payload(patch: &mut String, lines: &[String]) -> Result<(), FunctionCallError> {
    for line in lines {
        if line.contains(['\r', '\n']) {
            return model_error("transaction line values must not contain line endings");
        }
        patch.push('\n');
        patch.push('+');
        patch.push_str(line);
    }
    Ok(())
}

struct Observed {
    bytes: Vec<u8>,
    text: String,
}

fn observe(root: &Path, model_path: &str) -> Result<Observed, FunctionCallError> {
    let path = resolve_existing(root, model_path)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| io_error("inspect", &path, error))?;
    if !metadata.file_type().is_file() {
        return model_error(format!("Hashline path {model_path} is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return model_error(format!(
                "Hashline path {model_path} has multiple hard links"
            ));
        }
    }
    if metadata.len() > MAX_FILE_BYTES {
        return model_error(format!(
            "Hashline file exceeds the {MAX_FILE_BYTES}-byte limit"
        ));
    }
    let bytes = fs::read(&path).map_err(|error| io_error("read", &path, error))?;
    if bytes.contains(&0) {
        return model_error(format!(
            "Hashline path {model_path} contains NUL/binary content"
        ));
    }
    let text = String::from_utf8(bytes.clone()).map_err(|_| {
        FunctionCallError::RespondToModel(format!("Hashline path {model_path} is not valid UTF-8"))
    })?;
    Ok(Observed { bytes, text })
}

fn validate_model_path(model_path: &str) -> Result<(), FunctionCallError> {
    let path = Path::new(model_path);
    if model_path.is_empty() || path.is_absolute() {
        return model_error("Hashline paths must be non-empty and workspace-relative");
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return model_error(format!(
            "Hashline path {model_path} contains an invalid component"
        ));
    }
    Ok(())
}

fn resolve_root(workspace: &Path, root: &str) -> Result<PathBuf, FunctionCallError> {
    if root == "." {
        return workspace
            .canonicalize()
            .map_err(|error| io_error("open workspace", workspace, error));
    }
    resolve_existing(workspace, root).and_then(|path| {
        if path.is_dir() {
            Ok(path)
        } else {
            model_error(format!("transaction root {root} is not a directory"))
        }
    })
}

fn resolve_existing(root: &Path, model_path: &str) -> Result<PathBuf, FunctionCallError> {
    validate_model_path(model_path)?;
    let root = root
        .canonicalize()
        .map_err(|error| io_error("open root", root, error))?;
    let mut current = root.clone();
    for component in Path::new(model_path).components() {
        let Component::Normal(part) = component else {
            unreachable!()
        };
        current.push(part);
        let metadata =
            fs::symlink_metadata(&current).map_err(|error| io_error("resolve", &current, error))?;
        if metadata.file_type().is_symlink() {
            return model_error(format!(
                "Hashline path {model_path} traverses a symbolic link"
            ));
        }
    }
    Ok(current)
}

fn resolve_destination(
    root: &Path,
    model_path: &str,
    create_parents: bool,
) -> Result<PathBuf, FunctionCallError> {
    validate_model_path(model_path)?;
    let parent = Path::new(model_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let root = root
        .canonicalize()
        .map_err(|error| io_error("open root", root, error))?;
    let parent_path = root.join(parent);
    if create_parents {
        fs::create_dir_all(&parent_path)
            .map_err(|error| io_error("create parent", &parent_path, error))?;
    }
    let canonical_parent = parent_path
        .canonicalize()
        .map_err(|error| io_error("resolve parent", &parent_path, error))?;
    if !canonical_parent.starts_with(&root) {
        return model_error(format!("Hashline path {model_path} escapes the workspace"));
    }
    Ok(root.join(model_path))
}

fn validate_transaction_parent(root: &Path, model_path: &str) -> Result<(), FunctionCallError> {
    let parent = Path::new(model_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    if parent == Path::new(".") {
        return Ok(());
    }
    let parent_text = parent.to_str().ok_or_else(|| {
        FunctionCallError::RespondToModel("transaction parent path is not UTF-8".to_owned())
    })?;
    let resolved = resolve_existing(root, parent_text)?;
    if !resolved.is_dir() {
        return model_error(format!(
            "transaction parent {} is not a directory",
            parent.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_transaction_capability(root: &Path) -> Result<(), FunctionCallError> {
    use nix::sys::statfs::{EXT4_SUPER_MAGIC, TMPFS_MAGIC};

    let filesystem = nix::sys::statfs::statfs(root)
        .map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "unsupported: failed to inspect transaction filesystem: {error}"
            ))
        })?
        .filesystem_type();
    if matches!(filesystem, EXT4_SUPER_MAGIC | TMPFS_MAGIC) {
        Ok(())
    } else {
        model_error(format!(
            "unsupported: durable Hashline transactions require a proven Linux ext-family or tmpfs filesystem; found {filesystem:?}"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn ensure_transaction_capability(_root: &Path) -> Result<(), FunctionCallError> {
    model_error(
        "unsupported: durable Hashline transactions currently require Linux ext-family or tmpfs filesystem semantics",
    )
}

#[cfg(target_os = "linux")]
fn acquire_transaction_lease(root: &Path) -> Result<nix::fcntl::Flock<File>, FunctionCallError> {
    let directory =
        File::open(root).map_err(|error| io_error("open transaction root", root, error))?;
    nix::fcntl::Flock::lock(directory, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
        |(_, error)| {
            FunctionCallError::RespondToModel(format!(
                "transaction conflict: another commit owns the selected root: {error}"
            ))
        },
    )
}

#[cfg(not(target_os = "linux"))]
fn acquire_transaction_lease(_root: &Path) -> Result<File, FunctionCallError> {
    model_error("unsupported: transaction coordination requires Linux")
}

fn ensure_missing(root: &Path, model_path: &str) -> Result<(), FunctionCallError> {
    validate_model_path(model_path)?;
    let target = root.join(model_path);
    match fs::symlink_metadata(&target) {
        Ok(_) => model_error(format!("Hashline destination {model_path} already exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect", &target, error)),
    }
}

fn reject_conflicts(prepared: &[PreparedMutation]) -> Result<(), FunctionCallError> {
    let mut paths = BTreeSet::new();
    for mutation in prepared {
        let mut insert = |path: &str| {
            if paths.insert(path.to_owned()) {
                Ok(())
            } else {
                model_error(format!("Hashline request uses path {path} more than once"))
            }
        };
        match mutation {
            PreparedMutation::Write { path, .. } | PreparedMutation::Delete { path, .. } => {
                insert(path)?;
            }
            PreparedMutation::Move {
                source,
                destination,
                ..
            } => {
                insert(source)?;
                insert(destination)?;
            }
        }
    }
    Ok(())
}

fn apply_prepared(root: &Path, prepared: &[PreparedMutation]) -> Result<(), FunctionCallError> {
    let mut applied = Vec::new();
    for mutation in prepared {
        if let Err(error) = apply_one(root, mutation) {
            let rollback_ok = applied
                .iter()
                .rev()
                .all(|item| rollback_one(root, item).is_ok());
            return model_error(format!(
                "Hashline mutation failed: {error}; rollback_restored_all={rollback_ok}"
            ));
        }
        applied.push(mutation.clone());
    }
    Ok(())
}

fn apply_one(root: &Path, mutation: &PreparedMutation) -> Result<(), FunctionCallError> {
    match mutation {
        PreparedMutation::Write {
            path,
            before,
            after,
        } => {
            if let Some(expected) = before {
                validate_exact(path, &observe(root, path)?.bytes, &exact_digest(expected))?;
            } else {
                ensure_missing(root, path)?;
            }
            atomic_write(root, path, after, before.is_none())
        }
        PreparedMutation::Delete { path, before } => {
            validate_exact(path, &observe(root, path)?.bytes, &exact_digest(before))?;
            let target = resolve_existing(root, path)?;
            fs::remove_file(&target).map_err(|error| io_error("delete", &target, error))?;
            sync_parent(&target)
        }
        PreparedMutation::Move {
            source,
            destination,
            before,
            after,
        } => {
            validate_exact(source, &observe(root, source)?.bytes, &exact_digest(before))?;
            ensure_missing(root, destination)?;
            atomic_write(root, destination, after, true)?;
            let source_path = resolve_existing(root, source)?;
            fs::remove_file(&source_path)
                .map_err(|error| io_error("remove move source", &source_path, error))?;
            sync_parent(&source_path)
        }
    }
}

fn rollback_one(root: &Path, mutation: &PreparedMutation) -> Result<(), FunctionCallError> {
    match mutation {
        PreparedMutation::Write {
            path,
            before: Some(before),
            ..
        } => atomic_write(root, path, before, false),
        PreparedMutation::Write {
            path, before: None, ..
        } => {
            let target = resolve_existing(root, path)?;
            fs::remove_file(&target)
                .map_err(|error| io_error("rollback create", &target, error))?;
            sync_parent(&target)
        }
        PreparedMutation::Delete { path, before } => atomic_write(root, path, before, true),
        PreparedMutation::Move {
            source,
            destination,
            before,
            ..
        } => {
            if let Ok(target) = resolve_existing(root, destination) {
                fs::remove_file(&target)
                    .map_err(|error| io_error("rollback move destination", &target, error))?;
                sync_parent(&target)?;
            }
            atomic_write(root, source, before, true)
        }
    }
}

fn atomic_write(
    root: &Path,
    model_path: &str,
    contents: &[u8],
    create_parents: bool,
) -> Result<(), FunctionCallError> {
    let target = resolve_destination(root, model_path, create_parents)?;
    let parent = target
        .parent()
        .ok_or_else(|| FunctionCallError::RespondToModel("target has no parent".to_owned()))?;
    let temporary = parent.join(format!(
        ".nanocodex-hashline-{}-{}.tmp",
        std::process::id(),
        exact_digest(model_path.as_bytes())
    ));
    let mut file =
        File::create(&temporary).map_err(|error| io_error("stage", &temporary, error))?;
    if let Ok(metadata) = fs::metadata(&target) {
        file.set_permissions(metadata.permissions())
            .map_err(|error| io_error("preserve permissions", &temporary, error))?;
    }
    file.write_all(contents)
        .map_err(|error| io_error("write stage", &temporary, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync stage", &temporary, error))?;
    fs::rename(&temporary, &target).map_err(|error| io_error("replace", &target, error))?;
    sync_parent(&target)?;
    if fs::read(&target).map_err(|error| io_error("verify", &target, error))? != contents {
        return model_error(format!("post-write verification failed for {model_path}"));
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), FunctionCallError> {
    let parent = path.parent().ok_or_else(|| {
        FunctionCallError::RespondToModel(format!("{} has no parent directory", path.display()))
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync parent directory", parent, error))
}

#[derive(Serialize, Deserialize)]
struct Journal {
    mutations: Vec<JournalMutation>,
}

#[derive(Serialize, Deserialize)]
struct JournalMutation {
    kind: String,
    path: String,
    destination: Option<String>,
    before: Option<String>,
}

fn journal_dir(root: &Path) -> PathBuf {
    root.join(".nanocodex/hashline-transactions")
}

fn write_journal(
    root: &Path,
    id: &str,
    prepared: &[PreparedMutation],
) -> Result<(), FunctionCallError> {
    let directory = journal_dir(root);
    fs::create_dir_all(&directory)
        .map_err(|error| io_error("create transaction storage", &directory, error))?;
    let mutations = prepared
        .iter()
        .map(|mutation| match mutation {
            PreparedMutation::Write { path, before, .. } => JournalMutation {
                kind: "write".to_owned(),
                path: path.clone(),
                destination: None,
                before: before.as_deref().map(encode_bytes),
            },
            PreparedMutation::Delete { path, before } => JournalMutation {
                kind: "delete".to_owned(),
                path: path.clone(),
                destination: None,
                before: Some(encode_bytes(before)),
            },
            PreparedMutation::Move {
                source,
                destination,
                before,
                ..
            } => JournalMutation {
                kind: "move".to_owned(),
                path: source.clone(),
                destination: Some(destination.clone()),
                before: Some(encode_bytes(before)),
            },
        })
        .collect();
    let bytes = serde_json::to_vec(&Journal { mutations })
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    atomic_write(&directory, &format!("{id}.json"), &bytes, true)
}

fn recover_pending(root: &Path) -> Result<(), FunctionCallError> {
    let directory = journal_dir(root);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("scan transaction storage", &directory, error)),
    };
    for entry in entries.take(MAX_MUTATIONS) {
        let entry =
            entry.map_err(|error| io_error("scan transaction storage", &directory, error))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            return model_error("transaction storage contains an unrecognized artifact");
        }
        let journal: Journal = serde_json::from_slice(
            &fs::read(&path).map_err(|error| io_error("read journal", &path, error))?,
        )
        .map_err(|error| {
            FunctionCallError::RespondToModel(format!("invalid transaction journal: {error}"))
        })?;
        for mutation in journal.mutations.iter().rev() {
            match (mutation.kind.as_str(), mutation.before.as_deref()) {
                ("write" | "delete", Some(before)) => {
                    atomic_write(root, &mutation.path, &decode_bytes(before)?, true)?;
                }
                ("write", None) => {
                    if let Ok(target) = resolve_existing(root, &mutation.path) {
                        fs::remove_file(&target)
                            .map_err(|error| io_error("recover create", &target, error))?;
                        sync_parent(&target)?;
                    }
                }
                ("move", Some(before)) => {
                    if let Some(destination) = &mutation.destination
                        && let Ok(target) = resolve_existing(root, destination)
                    {
                        fs::remove_file(&target)
                            .map_err(|error| io_error("recover move", &target, error))?;
                        sync_parent(&target)?;
                    }
                    atomic_write(root, &mutation.path, &decode_bytes(before)?, true)?;
                }
                _ => return model_error("transaction journal contains an invalid mutation"),
            }
        }
        fs::remove_file(&path)
            .map_err(|error| io_error("remove recovered journal", &path, error))?;
        sync_parent(&path)?;
    }
    cleanup_journal_dirs(root)
}

fn remove_journal(root: &Path, id: &str) -> Result<(), FunctionCallError> {
    let path = journal_dir(root).join(format!("{id}.json"));
    fs::remove_file(&path).map_err(|error| io_error("remove transaction journal", &path, error))?;
    sync_parent(&path)?;
    cleanup_journal_dirs(root)
}

fn cleanup_journal_dirs(root: &Path) -> Result<(), FunctionCallError> {
    let directory = journal_dir(root);
    if directory
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_none())
    {
        fs::remove_dir(&directory)
            .map_err(|error| io_error("remove transaction storage", &directory, error))?;
        let parent = root.join(".nanocodex");
        if parent
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_none())
        {
            fs::remove_dir(&parent)
                .map_err(|error| io_error("remove empty state directory", &parent, error))?;
        }
    }
    Ok(())
}

fn prepared_digests(prepared: &[PreparedMutation]) -> Vec<Value> {
    prepared
        .iter()
        .map(|mutation| match mutation {
            PreparedMutation::Write {
                path,
                before,
                after,
            } => json!([
                "write",
                path,
                before.as_deref().map(exact_digest),
                exact_digest(after)
            ]),
            PreparedMutation::Delete { path, before } => {
                json!(["delete", path, exact_digest(before)])
            }
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
            } => json!([
                "move",
                source,
                destination,
                exact_digest(before),
                exact_digest(after)
            ]),
        })
        .collect()
}

fn mutation_bytes(mutation: &PreparedMutation) -> usize {
    match mutation {
        PreparedMutation::Write { before, after, .. } => {
            before.as_ref().map_or(0, Vec::len) + after.len()
        }
        PreparedMutation::Delete { before, .. } => before.len(),
        PreparedMutation::Move { before, after, .. } => before.len() + after.len(),
    }
}

fn validate_exact(path: &str, bytes: &[u8], expected: &str) -> Result<(), FunctionCallError> {
    let actual = exact_digest(bytes);
    if expected.len() != 64 || !expected.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return model_error(format!(
            "invalid exactDigest for {path}; expected 64 lowercase hex characters"
        ));
    }
    if actual != expected.to_ascii_lowercase() {
        return model_error(format!(
            "exactDigest mismatch for {path}: expected {expected}, found {actual}; reread and rebuild the transaction"
        ));
    }
    Ok(())
}

fn exact_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn encode_bytes(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_bytes(encoded: &str) -> Result<Vec<u8>, FunctionCallError> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| {
            FunctionCallError::RespondToModel(format!("invalid recovery evidence: {error}"))
        })
}

fn str_from_bytes(bytes: &[u8]) -> Result<&str, FunctionCallError> {
    std::str::from_utf8(bytes).map_err(|_| {
        FunctionCallError::RespondToModel("prepared Hashline contents are not UTF-8".to_owned())
    })
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(operation: &str, path: &Path, error: std::io::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("failed to {operation} {}: {error}", path.display()))
}

fn model_error<T>(message: impl Into<String>) -> Result<T, FunctionCallError> {
    Err(FunctionCallError::RespondToModel(message.into()))
}

fn read_definition() -> ToolDefinition {
    ToolDefinition::function(
        "hashline__read",
        "Read a bounded UTF-8 file range with compact file/line anchors and an exact-byte SHA-256 digest.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "minimum": 1},
                "end_line": {"type": "integer", "minimum": 1},
                "max_lines": {"type": "integer", "minimum": 1, "maximum": HARD_READ_MAX_LINES}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn block_definition() -> ToolDefinition {
    ToolDefinition::function(
        "hashline__find_block",
        "Resolve a recent Hashline line or block anchor to a reproducible language-aware block and bounded excerpt.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "anchor": {"type": "string"},
                "max_lines": {"type": "integer", "minimum": 1, "maximum": HARD_BLOCK_MAX_LINES}
            },
            "required": ["path", "anchor"],
            "additionalProperties": false
        }),
    )
}

fn patch_definition() -> ToolDefinition {
    ToolDefinition::function(
        "hashline__patch",
        "Apply a complete hash-anchored routine patch. Supports line/range/block edits, sectioned creates, REM, MV, dry runs, and validation before the first write. Routine multi-file commits are not crash-atomic; use hashline__transaction for recoverable batches.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "patch": {"type": "string"},
                "dry_run": {"type": "boolean"},
                "create": {"type": "boolean"}
            },
            "required": ["path", "patch"],
            "additionalProperties": false
        }),
    )
}

fn transaction_definition() -> ToolDefinition {
    ToolDefinition::function(
        "hashline__transaction",
        "Preview, immediately commit, or commit only an exact previously previewed bounded multi-file Hashline transaction with retained restart-recovery evidence.",
        json!({
            "type": "object",
            "properties": {
                "action": {"oneOf": [
                    {"type": "object", "properties": {"type": {"const": "preview"}}, "required": ["type"], "additionalProperties": false},
                    {"type": "object", "properties": {"type": {"const": "commit"}}, "required": ["type"], "additionalProperties": false},
                    {"type": "object", "properties": {"type": {"const": "commitPreviewed"}, "expectedPlanDigest": {"type": "string"}}, "required": ["type", "expectedPlanDigest"], "additionalProperties": false}
                ]},
                "root": {"type": "string"},
                "mutations": {"type": "array", "minItems": 1, "maxItems": MAX_MUTATIONS, "items": mutation_schema()}
            },
            "required": ["action", "mutations"],
            "additionalProperties": false
        }),
    )
}

fn mutation_schema() -> Value {
    let anchor = json!({"type": "object", "properties": {"line": {"type": "integer", "minimum": 1}, "expectedHash": {"type": "string"}}, "required": ["line", "expectedHash"], "additionalProperties": false});
    let expected = json!({"type": "object", "properties": {"exactDigest": {"type": "string"}}, "required": ["exactDigest"], "additionalProperties": false});
    let edits = json!({"type": "array", "items": {"oneOf": [
        {"type": "object", "properties": {"type": {"const": "replaceAll"}, "contents": {"type": "string"}}, "required": ["type", "contents"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "replaceLines"}, "range": {"type": "object", "properties": {"start": anchor.clone(), "end": anchor.clone()}, "required": ["start", "end"], "additionalProperties": false}, "lines": {"type": "array", "items": {"type": "string"}}}, "required": ["type", "range", "lines"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "insertBefore"}, "anchor": anchor.clone(), "lines": {"type": "array", "items": {"type": "string"}}}, "required": ["type", "anchor", "lines"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "insertAfter"}, "anchor": anchor, "lines": {"type": "array", "items": {"type": "string"}}}, "required": ["type", "anchor", "lines"], "additionalProperties": false}
    ]}});
    json!({"oneOf": [
        {"type": "object", "properties": {"type": {"const": "create"}, "path": {"type": "string"}, "contents": {"type": "string"}}, "required": ["type", "path", "contents"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "update"}, "path": {"type": "string"}, "expected": expected.clone(), "edits": edits.clone()}, "required": ["type", "path", "expected", "edits"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "delete"}, "path": {"type": "string"}, "expected": expected.clone()}, "required": ["type", "path", "expected"], "additionalProperties": false},
        {"type": "object", "properties": {"type": {"const": "move"}, "source": {"type": "string"}, "expected": expected, "destination": {"type": "string"}, "edits": edits}, "required": ["type", "source", "expected", "destination", "edits"], "additionalProperties": false}
    ]})
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
