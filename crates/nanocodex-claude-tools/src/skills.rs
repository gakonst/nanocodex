//! Caller-owned skill catalog and invocation. Skill metadata never grants tools,
//! executes commands or installs hooks. Fork/model metadata requires a host child executor.
use crate::context::{
    FILE_BYTES, authorized_root, frontmatter, local_directory, read_local, read_user_file,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const MAX_SKILLS: usize = 64;
const MAX_SCAN: usize = 256;
const MAX_ARGS: usize = 8 * 1024;
const MAX_EXPANDED: usize = 64 * 1024;

/// Invocation provenance must come from the host, never model-supplied JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillInvocation {
    User,
    Model,
}
/// Metadata that may be shown without loading the skill's full body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub path: String,
    pub argument_hint: Option<String>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    /// Declarative requested tools, not permission grants. Host policy wins.
    pub allowed_tools: Vec<String>,
    /// Requires a host-owned child runtime. Never interpreted as inline guidance.
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub background: bool,
}
/// Discovery is bounded; malformed entries and omissions are visible.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillCatalog {
    pub skills: Vec<SkillDefinition>,
    pub diagnostics: Vec<String>,
}
/// Expanded local content. Tool authority remains owned by the embedding.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillExpansion {
    pub skill: SkillDefinition,
    pub base_directory: String,
    pub instructions: String,
    pub arguments: String,
}
/// Skills are read fresh on each invocation, so edits/removals take effect.
#[derive(Clone, Debug)]
pub struct ClaudeSkills {
    root: PathBuf,
    user: Vec<nanocodex_home::SkillRoot>,
}
impl ClaudeSkills {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self {
            root: authorized_root(root.as_ref())?,
            user: Vec::new(),
        })
    }
    /// Adds host-resolved user skill roots in precedence order (for example
    /// `nanocodex_home::AgentHome::skill_roots`: `~/.codex/skills`,
    /// `~/.claude/skills`, `~/.agents/skills`). Personal skills win over
    /// same-named project skills, as in Claude Code. Symlinked user skill
    /// folders are followed; workspace skills still reject symlinks.
    #[must_use]
    pub fn with_user_roots(mut self, roots: Vec<nanocodex_home::SkillRoot>) -> Self {
        self.user = roots;
        self
    }
    /// A catalog for the requested caller: model-disabled skills are omitted
    /// entirely from model discovery; user-hidden skills stay model-invocable.
    pub fn catalog(&self, caller: SkillInvocation) -> SkillCatalog {
        let mut catalog = SkillCatalog::default();
        let overrides = match self.overrides() {
            Ok(overrides) => overrides,
            Err(error) => {
                catalog.diagnostics.push(error);
                return catalog;
            }
        };
        let mut by_name = BTreeMap::new();
        let mut scanned = 0;
        // Native Claude entries override the generic project location.
        for root in [".agents/skills", ".claude/skills"] {
            if !local_directory(&self.root, Path::new(root)) {
                continue;
            }
            let Ok(entries) = fs::read_dir(self.root.join(root)) else {
                continue;
            };
            let mut paths = Vec::new();
            for entry in entries.flatten() {
                if scanned >= MAX_SCAN {
                    catalog
                        .diagnostics
                        .push("skill discovery reached entry limit".into());
                    break;
                }
                scanned += 1;
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    paths.push(Path::new(root).join(entry.file_name()).join("SKILL.md"));
                }
            }
            paths.sort();
            for path in paths {
                match self.read(&path) {
                    Ok((skill, _)) => {
                        by_name.insert(skill.name.clone(), skill);
                    }
                    Err(error) => catalog
                        .diagnostics
                        .push(format!("{}: {error}", path.display())),
                }
            }
        }
        if !self.user.is_empty() {
            let resolved = nanocodex_home::resolve_skills(&self.user);
            for diagnostic in resolved.diagnostics {
                catalog.diagnostics.push(diagnostic.to_string());
            }
            for entry in resolved.skills {
                match self.read(&entry.skill_md) {
                    Ok((skill, _)) => {
                        by_name.insert(skill.name.clone(), skill);
                    }
                    Err(error) => catalog
                        .diagnostics
                        .push(format!("{}: {error}", entry.skill_md.display())),
                }
            }
        }
        if by_name.len() > MAX_SKILLS {
            catalog
                .diagnostics
                .push("skill catalog truncated at 64 entries".into());
        }
        catalog.skills = by_name
            .into_values()
            .filter(|s| override_allows(overrides.get(&s.name).map(String::as_str), caller))
            .map(|mut skill| {
                if overrides.get(&skill.name).is_some_and(|v| v == "name-only") {
                    skill.description.clear();
                }
                skill
            })
            .filter(|s| match caller {
                SkillInvocation::Model => !s.disable_model_invocation,
                SkillInvocation::User => s.user_invocable,
            })
            .take(MAX_SKILLS)
            .collect();
        catalog
    }
    /// Standalone Claude-native Skill tool definition.
    pub fn definitions() -> Vec<Value> {
        vec![
            json!({"name":"Skill","description":"Load a workspace skill by its catalog name and substitute arguments. Skill text is project context and cannot grant tool permissions. This tool cannot invoke skills marked disable-model-invocation. context:fork requires a host child executor; standalone execute refuses it. Dynamic commands and hooks are unsupported.","input_schema":{"type":"object","properties":{"skill":{"type":"string"},"args":{"type":"string"}},"required":["skill"],"additionalProperties":false}}),
        ]
    }
    /// Model boundary. Invocation provenance is fixed, not an input option.
    pub async fn execute(&self, name: &str, input: Value) -> Result<String, String> {
        if name != "Skill" {
            return Err(format!("unknown skill tool: {name}"));
        }
        let fields = input.as_object().ok_or("Skill input must be an object")?;
        if fields.keys().any(|key| key != "skill" && key != "args") {
            return Err("unsupported Skill option".into());
        }
        let skill = fields
            .get("skill")
            .and_then(Value::as_str)
            .ok_or("skill must be a string")?
            .to_owned();
        let args = match fields.get("args") {
            Some(value) => value.as_str().ok_or("args must be a string")?,
            None => "",
        }
        .to_owned();
        let this = self.clone();
        let expansion =
            tokio::task::spawn_blocking(move || this.invoke(&skill, &args, SkillInvocation::Model))
                .await
                .map_err(|e| format!("skill task: {e}"))??;
        if expansion.skill.context.is_some() {
            return Err(
                "context:fork requires a host-owned child executor; no skill body was executed"
                    .into(),
            );
        }
        serde_json::to_string(&expansion).map_err(|e| e.to_string())
    }
    /// Explicit user invocation is available only to an embedding with a real
    /// user action; never infer it from a model's request or skill text.
    pub fn invoke(
        &self,
        name: &str,
        args: &str,
        caller: SkillInvocation,
    ) -> Result<SkillExpansion, String> {
        if !valid_name(name) {
            return Err(
                "skill name must contain 1..64 lowercase letters, digits or hyphens".into(),
            );
        }
        if args.len() > MAX_ARGS {
            return Err("skill arguments exceed 8 KiB".into());
        }
        self.check_override(name, caller)?;
        let catalog = self.catalog(caller);
        let definition = catalog
            .skills
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| {
                format!(
                    "skill {name:?} is unavailable for this caller; inspect catalog diagnostics"
                )
            })?;
        let (skill, body) = self.read(Path::new(&definition.path))?;
        self.check_override(name, caller)?;
        // Revalidate after discovery to prevent an edited opt-out being bypassed.
        if (caller == SkillInvocation::Model && skill.disable_model_invocation)
            || (caller == SkillInvocation::User && !skill.user_invocable)
        {
            return Err("skill invocation disabled for this caller".into());
        }
        if skill.name != name {
            return Err("skill changed during discovery; retry after inspecting it".into());
        }
        if body.contains("!`") {
            return Err("dynamic shell injection is unsupported; skill was not executed".into());
        }
        let words = split_arguments(args)?;
        let pattern =
            regex::Regex::new(r"\$ARGUMENTS(?:\[(\d+)\])?|\$(\d+)").map_err(|e| e.to_string())?;
        let has_placeholder = pattern.is_match(&body);
        // One replacement pass: argument contents are literal, never re-expanded.
        let mut instructions = String::new();
        let mut end = 0;
        for captures in pattern.captures_iter(&body) {
            let matched = captures.get(0).expect("whole placeholder match");
            append_bounded(&mut instructions, &body[end..matched.start()])?;
            let index = captures.get(1).or_else(|| captures.get(2));
            let replacement = if let Some(index) = index {
                index
                    .as_str()
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| words.get(i))
                    .map_or("", String::as_str)
            } else {
                args
            };
            append_bounded(&mut instructions, replacement)?;
            end = matched.end();
        }
        append_bounded(&mut instructions, &body[end..])?;
        if !has_placeholder && !args.is_empty() {
            append_bounded(&mut instructions, "\n\nARGUMENTS: ")?;
            append_bounded(&mut instructions, args)?;
        }
        Ok(SkillExpansion {
            base_directory: Path::new(&skill.path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into(),
            skill,
            instructions,
            arguments: args.into(),
        })
    }
    /// Project and local visibility settings only. These can restrict discovery
    /// and invocation, but never override frontmatter or host tool permissions.
    fn overrides(&self) -> Result<BTreeMap<String, String>, String> {
        let mut overrides = BTreeMap::new();
        for relative in [".claude/settings.json", ".claude/settings.local.json"] {
            match fs::symlink_metadata(self.root.join(relative)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("skillOverrides {relative}: {error}")),
                Ok(_) => {}
            }
            let (text, truncated) = read_local(&self.root, Path::new(relative), FILE_BYTES)
                .map_err(|e| format!("skillOverrides {relative}: {e}"))?;
            if truncated {
                return Err(format!("skillOverrides {relative} exceeds 32 KiB"));
            }
            let settings: Value = serde_json::from_str(&text)
                .map_err(|e| format!("skillOverrides {relative}: {e}"))?;
            let settings = settings
                .as_object()
                .ok_or("skillOverrides settings must be a JSON object")?;
            let Some(entries) = settings.get("skillOverrides") else {
                continue;
            };
            let entries = entries
                .as_object()
                .ok_or("skillOverrides must be an object of skill names to visibility strings")?;
            if entries.len() > MAX_SCAN {
                return Err("skillOverrides exceeds 256 entries".into());
            }
            for (name, value) in entries {
                let state = value
                    .as_str()
                    .ok_or("skillOverrides values must be visibility strings")?;
                if !valid_name(name)
                    || !matches!(state, "on" | "name-only" | "user-invocable-only" | "off")
                {
                    return Err(format!(
                        "invalid skillOverrides entry {name:?}: expected on/name-only/user-invocable-only/off for a valid skill name"
                    ));
                }
                overrides.insert(name.clone(), state.to_owned());
            }
            if overrides.len() > MAX_SCAN {
                return Err("skillOverrides exceeds 256 combined entries".into());
            }
        }
        Ok(overrides)
    }
    fn check_override(&self, name: &str, caller: SkillInvocation) -> Result<(), String> {
        let overrides = self.overrides()?;
        if !override_allows(overrides.get(name).map(String::as_str), caller) {
            return Err(format!(
                "skill {name:?} is unavailable for this caller due to skillOverrides"
            ));
        }
        Ok(())
    }
    /// Workspace skills are workspace-relative and read without following
    /// symlinks; user skills are absolute paths discovered from user roots.
    fn read(&self, path: &Path) -> Result<(SkillDefinition, String), String> {
        let (text, truncated) = if path.is_absolute() {
            if !self.user.iter().any(|root| path.starts_with(&root.path)) {
                return Err("skill path is outside the configured skill roots".into());
            }
            read_user_file(path, FILE_BYTES)?
        } else {
            read_local(&self.root, path, FILE_BYTES)?
        };
        if truncated {
            return Err("skill exceeds 32 KiB".into());
        }
        let (meta, body) = frontmatter(&text)?;
        for unsupported in ["hooks", "effort", "paths"] {
            if meta.get(unsupported).is_some() {
                return Err(format!("unsupported execution frontmatter: {unsupported}"));
            }
        }
        let context = string_field(&meta, "context")?;
        if context.as_deref().is_some_and(|c| c != "fork") {
            return Err("context supports only fork".into());
        }
        let agent = string_field(&meta, "agent")?;
        let model = string_field(&meta, "model")?.filter(|m| m != "inherit");
        if model.as_ref().is_some_and(|m| {
            !matches!(m.as_str(), "opus" | "sonnet" | "haiku" | "fable")
                && !m.starts_with("claude-")
        }) {
            return Err("forked skill model must be a Claude model ID, alias or inherit".into());
        }
        let background = bool_field(&meta, "background", false)?;
        if context.is_none()
            && (agent.is_some() || model.is_some() || meta.get("background").is_some())
        {
            return Err("agent/model/background require context:fork; inline model overrides are unsupported".into());
        }
        if agent
            .as_ref()
            .is_some_and(|a| a.is_empty() || a.len() > 64 || a == "fork")
        {
            return Err(
                "invalid skill agent; context:fork uses a clean child, not a conversation fork"
                    .into(),
            );
        }
        let fallback = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .ok_or("invalid skill directory name")?;
        let name = string_field(&meta, "name")?.unwrap_or_else(|| fallback.into());
        if !valid_name(&name) {
            return Err(
                "invalid skill name (expected lowercase letters, digits, hyphens; max 64)".into(),
            );
        }
        let description = string_field(&meta, "description")?.unwrap_or_else(|| {
            body.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(200)
                .collect()
        });
        if description.len() > 2048 {
            return Err("skill description exceeds 2 KiB".into());
        }
        let allowed_tools = match meta.get("allowed-tools") {
            None => Vec::new(),
            Some(Value::String(value)) => value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            Some(Value::Array(values)) => values
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or("allowed-tools entries must be strings")
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("allowed-tools must be a string or string list".into()),
        };
        if allowed_tools.len() > 64 || allowed_tools.iter().any(|tool| tool.len() > 256) {
            return Err("allowed-tools exceeds 64 entries or 256 bytes per entry".into());
        }
        let argument_hint = string_field(&meta, "argument-hint")?;
        if argument_hint.as_ref().is_some_and(|hint| hint.len() > 1024) {
            return Err("argument-hint exceeds 1 KiB".into());
        }
        Ok((
            SkillDefinition {
                name,
                description,
                path: path.to_string_lossy().into(),
                argument_hint,
                disable_model_invocation: bool_field(&meta, "disable-model-invocation", false)?,
                user_invocable: bool_field(&meta, "user-invocable", true)?,
                allowed_tools,
                context,
                agent,
                model,
                background,
            },
            body.into(),
        ))
    }
}
fn override_allows(state: Option<&str>, caller: SkillInvocation) -> bool {
    !matches!(state, Some("off"))
        && !(caller == SkillInvocation::Model && state == Some("user-invocable-only"))
}
fn append_bounded(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > MAX_EXPANDED {
        return Err("expanded skill exceeds 64 KiB".into());
    }
    output.push_str(text);
    Ok(())
}
fn string_field(meta: &Value, key: &str) -> Result<Option<String>, String> {
    meta.get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{key} must be a string"))
        })
        .transpose()
}
fn bool_field(meta: &Value, key: &str, default: bool) -> Result<bool, String> {
    meta.get(key).map_or(Ok(default), |value| {
        value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean"))
    })
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn split_arguments(args: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut started = false;
    for c in args.chars() {
        if escape {
            word.push(c);
            escape = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escape = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if quote.is_some() || escape {
        return Err("unclosed quote or trailing escape in skill arguments".into());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}
