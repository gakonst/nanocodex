//! Provider-backed skill catalog and bounded resource reads.
use super::{ExtensionProvider, error};
use crate::{ToolContext, ToolOutput, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authority {
    Orchestrator,
    Executor { id: String },
}
#[derive(Clone, Debug, Serialize)]
pub struct ListedSkill {
    pub authority: Authority,
    pub package: String,
    pub name: String,
    pub description: String,
    pub main_resource: String,
}
/// One explicitly registered package. Resources are exact skill:// identifiers;
/// executor files are resolved under this package's canonical root.
pub struct SkillPackage {
    pub metadata: ListedSkill,
    pub resources: BTreeMap<String, String>,
    pub executor_root: Option<PathBuf>,
}
pub struct SkillTools {
    packages: Vec<SkillPackage>,
    snapshot: Mutex<Option<(String, String, String)>>,
}
impl SkillTools {
    pub fn new(packages: Vec<SkillPackage>) -> Self {
        Self {
            packages,
            snapshot: Mutex::new(None),
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Selector {
    Orchestrator,
    Executor,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    authority: Selector,
    cursor: Option<String>,
}
#[derive(Deserialize)]
struct ReadArgs {
    package: String,
    resource: Option<String>,
    cursor: Option<String>,
}
fn fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
fn cursor(value: &str, index: usize) -> String {
    format!("{:016x}:{index}", fingerprint(value))
}
fn offset(value: &str, input: Option<&str>) -> Result<usize, crate::contract::ToolError> {
    let Some(input) = input else { return Ok(0) };
    let (hash, index) = input
        .split_once(':')
        .ok_or_else(|| error("skill cursor is invalid"))?;
    if hash != format!("{:016x}", fingerprint(value)) {
        return Err(error("skill cursor is stale"));
    }
    Ok(index.parse()?)
}
async fn read_executor(root: &Path, relative: &str) -> Result<String, crate::contract::ToolError> {
    if relative.is_empty()
        || Path::new(relative).is_absolute()
        || relative
            .split('/')
            .any(|p| p.is_empty() || p == ".." || p == ".")
    {
        return Err(error("invalid skill resource path"));
    }
    let root = tokio::fs::canonicalize(root).await?;
    let path = tokio::fs::canonicalize(root.join(relative)).await?;
    if !path.starts_with(&root) {
        return Err(error("skill resource escapes package"));
    }
    Ok(tokio::fs::read_to_string(path).await?)
}
#[async_trait::async_trait]
impl ExtensionProvider for SkillTools {
    fn names(&self) -> &'static [&'static str] {
        &["skills__list", "skills__read"]
    }
    async fn execute(&self, name: &str, input: Value, _context: ToolContext<'_>) -> ToolResult {
        const BUDGET: usize = 512 * 1024;
        if name == "skills__list" {
            let a: ListArgs = serde_json::from_value(input)?;
            let entries: Vec<_> = self
                .packages
                .iter()
                .filter(|p| {
                    matches!(
                        (&a.authority, &p.metadata.authority),
                        (Selector::Orchestrator, Authority::Orchestrator)
                            | (Selector::Executor, Authority::Executor { .. })
                    )
                })
                .map(|p| &p.metadata)
                .collect();
            let encoded = serde_json::to_string(&entries)?;
            let start = offset(&encoded, a.cursor.as_deref())?;
            if start > entries.len() {
                return Err(error("skill cursor is invalid"));
            }
            let mut end = (start + 20).min(entries.len());
            loop {
                let response = json!({"skills": &entries[start..end], "warnings": [], "next_cursor": (end < entries.len()).then(|| cursor(&encoded, end))});
                if serde_json::to_vec(&response)?.len() <= BUDGET {
                    return Ok(ToolOutput::json(&response));
                }
                if end <= start + 1 {
                    return Err(error("skill metadata is too large to list"));
                }
                end -= 1;
            }
        }
        if name != "skills__read" {
            return Err(error("unknown skill tool"));
        }
        let a: ReadArgs = serde_json::from_value(input)?;
        if a.package.is_empty()
            || a.package.len() > 2048
            || a.resource
                .as_ref()
                .is_some_and(|r| r.is_empty() || r.len() > 2048)
        {
            return Err(error("invalid skill handle"));
        }
        let package = self
            .packages
            .iter()
            .find(|p| p.metadata.package == a.package)
            .ok_or_else(|| error("skill package is not available"))?;
        let resource = a
            .resource
            .unwrap_or_else(|| package.metadata.main_resource.clone());
        let binding = package
            .resources
            .get(&resource)
            .ok_or_else(|| error("skill resource is not available"))?;
        let mut snapshot = self.snapshot.lock().await;
        let contents = if a.cursor.is_some()
            && snapshot
                .as_ref()
                .is_some_and(|(p, r, _)| p == &a.package && r == &resource)
        {
            snapshot.as_ref().expect("matching snapshot").2.clone()
        } else if let Some(root) = &package.executor_root {
            read_executor(root, binding).await?
        } else {
            binding.clone()
        };
        let start = offset(&contents, a.cursor.as_deref())?;
        if start > contents.len() || !contents.is_char_boundary(start) {
            return Err(error("skill cursor is invalid"));
        }
        let response = |end| {
            let mut value = json!({"resource": resource, "contents": &contents[start..end], "next_cursor": (end < contents.len()).then(|| cursor(&contents,end))});
            if let (Authority::Executor { .. }, Some(root)) =
                (&package.metadata.authority, &package.executor_root)
            {
                value["skill_root"] = json!(root.to_string_lossy());
            }
            value
        };
        let mut end = contents.len();
        if serde_json::to_vec(&response(end))?.len() > BUDGET {
            let mut lower = start;
            let mut upper = end;
            while lower < upper {
                let probe = contents.ceil_char_boundary(lower.midpoint(upper).saturating_add(1));
                if serde_json::to_vec(&response(probe))?.len() <= BUDGET {
                    lower = probe;
                } else {
                    upper = contents.floor_char_boundary(probe.saturating_sub(1));
                }
            }
            end = lower;
            if end == start {
                return Err(error("skill response budget leaves no room for contents"));
            }
        }
        let output = ToolOutput::json(&response(end));
        *snapshot = Some((a.package, resource, contents));
        Ok(output)
    }
}
