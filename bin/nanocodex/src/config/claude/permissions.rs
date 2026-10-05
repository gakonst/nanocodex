//! Explicit native permission policy. This is tool admission, not OS isolation.
//! Syntax and precedence follow https://code.claude.com/docs/en/permissions.
//! Complex shell syntax fails conservatively; no classifier is claimed.
use eyre::{Result, WrapErr as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Read as _,
    path::{Component, Path, PathBuf},
};

pub(super) type WorkspaceResolver =
    std::sync::Arc<dyn Fn(&str) -> std::result::Result<PathBuf, String> + Send + Sync>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct Policy {
    #[serde(default)]
    pub(super) allow: Vec<String>,
    #[serde(default)]
    pub(super) ask: Vec<String>,
    #[serde(default)]
    pub(super) deny: Vec<String>,
    #[serde(default, rename = "defaultMode")]
    pub(super) mode: Option<String>,
    #[serde(skip)]
    pub(super) explicit: bool,
    #[serde(skip)]
    pub(super) replace_rules: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    permissions: Policy,
}
#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Allow,
    Ask,
    Deny(String),
}

impl Policy {
    pub(super) fn load(path: Option<&Path>, mode: Option<&str>) -> Result<Self> {
        let mut policy = if let Some(path) = path {
            let metadata =
                std::fs::metadata(path).wrap_err("cannot inspect --claude-permissions")?;
            if !metadata.is_file() || metadata.len() > 1024 * 1024 {
                bail!("--claude-permissions requires a regular JSON file at most 1 MiB");
            }
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                bail!("--claude-permissions exceeds 1 MiB");
            }
            serde_json::from_slice::<Settings>(&bytes)
                .wrap_err("invalid --claude-permissions JSON (expected {\"permissions\": {...}})")?
                .permissions
        } else {
            Self::default()
        };
        if let Some(mode) = mode {
            policy.mode = Some(mode.into());
        }
        policy.explicit = path.is_some() || mode.is_some();
        policy.replace_rules = path.is_some();
        // Selecting a rules file opts in to manual admission unless a mode was selected.
        if policy.explicit && policy.mode.is_none() {
            policy.mode = Some("default".into());
        }
        policy.validate()?;
        Ok(policy)
    }
    pub(super) fn validate(&self) -> Result<()> {
        if !matches!(
            self.mode.as_deref(),
            None | Some(
                "full-access"
                    | "bypassPermissions"
                    | "default"
                    | "manual"
                    | "acceptEdits"
                    | "plan"
                    | "dontAsk"
            )
        ) {
            bail!("unsupported permission mode; auto classifier mode is not implemented");
        }
        for (rules, allow) in [(&self.deny, false), (&self.ask, false), (&self.allow, true)] {
            for text in rules {
                Rule::parse(text, allow)?;
            }
        }
        Ok(())
    }
    /// Automatic imported project guidance must not bypass scoped read rules.
    pub(super) fn has_read_restrictions(&self) -> bool {
        self.deny
            .iter()
            .chain(&self.ask)
            .any(|rule| wild(rule.split('(').next().unwrap_or_default(), "Read", false))
    }
    /// Restricted parents cannot delegate to harnesses without a native policy bridge.
    pub(super) fn restricted(&self) -> bool {
        !self.allow.is_empty()
            || !self.ask.is_empty()
            || !self.deny.is_empty()
            || !matches!(
                self.mode.as_deref(),
                None | Some("full-access" | "bypassPermissions")
            )
    }
    pub(super) fn planning(&self) -> bool {
        self.mode.as_deref() == Some("plan")
    }
    pub(super) fn evaluate(&self, name: &str, input: &Value, workspace: &Path) -> Result<Decision> {
        for (rules, outcome, allow) in [
            (&self.deny, 0, false),
            (&self.ask, 1, false),
            (&self.allow, 2, true),
        ] {
            for text in rules {
                let rule = Rule::parse(text, allow)?;
                if rule.matches(name, input, workspace, allow) {
                    return Ok(match outcome {
                        0 => Decision::Deny(format!("permission denied by rule {text}")),
                        1 => Decision::Ask,
                        _ => Decision::Allow,
                    });
                }
            }
        }
        if matches!(
            self.mode.as_deref(),
            None | Some("full-access" | "bypassPermissions")
        ) {
            return Ok(Decision::Allow);
        }
        let file = input
            .get("file_path")
            .or_else(|| input.get("notebook_path"))
            .and_then(Value::as_str);
        let within =
            file.is_some_and(|p| paths(workspace, p).iter().all(|p| p.starts_with(workspace)));
        let read = matches!(name, "Read" | "Glob" | "Grep" | "ProjectContext")
            && input
                .get("file_path")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str)
                .is_none_or(|p| paths(workspace, p).iter().all(|p| p.starts_with(workspace)));
        let internal = matches!(
            name,
            "TaskGet"
                | "TaskList"
                | "TaskCreate"
                | "TaskUpdate"
                | "TodoWrite"
                | "AskUserQuestion"
                | "EnterPlanMode"
                | "ExitPlanMode"
                | "TaskOutput"
        );
        let edits = self.mode.as_deref() == Some("acceptEdits")
            && matches!(name, "Edit" | "Write" | "NotebookEdit")
            && within
            && file.is_some_and(|p| {
                !paths(workspace, p).iter().any(|p| {
                    p.components()
                        .any(|c| matches!(c.as_os_str().to_str(), Some(".git" | ".claude")))
                })
            });
        Ok(if read || internal || edits {
            Decision::Allow
        } else {
            Decision::Ask
        })
    }
}

struct Rule<'a> {
    tool: &'a str,
    spec: Option<&'a str>,
}
impl<'a> Rule<'a> {
    fn parse(text: &'a str, allow: bool) -> Result<Self> {
        let (tool, spec) = if let Some((tool, rest)) = text.split_once('(') {
            let spec = rest.strip_suffix(')').ok_or_else(|| {
                eyre::eyre!("invalid permission rule {text}: missing closing parenthesis")
            })?;
            if spec.is_empty() {
                bail!("invalid permission rule {text}: empty specifier");
            }
            (tool, Some(spec))
        } else {
            (text, None)
        };
        if tool.is_empty()
            || !tool
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_*-".contains(&b))
            || text.contains(['\n', '\r', '\0'])
        {
            bail!("invalid permission rule {text}");
        }
        if let Some(spec) = spec {
            if tool.contains('*') || tool.starts_with("mcp__") {
                bail!("specifier not supported for wildcard/MCP rule {text}");
            }
            match tool {
                "Bash" => {
                    if spec.starts_with("command:") {
                        bail!("use Bash(command text), not Bash(command:...)");
                    }
                }
                "Read" | "Edit" => {
                    if spec.starts_with('!') || spec.contains(['[', ']', '\\']) {
                        bail!(
                            "unsupported file permission pattern {text}; use literal paths, * and **"
                        );
                    }
                    if spec.split('/').any(|part| part == "..") {
                        bail!("permission paths cannot contain ..: {text}");
                    }
                }
                "WebFetch" if spec.starts_with("domain:") => {
                    if spec[7..].is_empty() || spec[7..].contains(['/', ':', '@', ' ']) {
                        bail!("invalid WebFetch domain rule {text}");
                    }
                }
                "Agent" | "Skill" => {}
                _ => {
                    if allow || !spec.contains(':') {
                        bail!("unsupported permission specifier {text}");
                    }
                }
            }
            if let Some((key, _)) = spec.split_once(':')
                && matches!(
                    key.trim(),
                    "command" | "file_path" | "notebook_path" | "path" | "url"
                )
            {
                bail!("unsupported primary-parameter permission rule {text}");
            }
        }
        Ok(Self { tool, spec })
    }
    fn matches(&self, name: &str, input: &Value, cwd: &Path, allow: bool) -> bool {
        let file_alias = (self.tool == "Edit" && matches!(name, "Write" | "NotebookEdit"))
            || (self.tool == "Read"
                && matches!(name, "Glob" | "Grep" | "Edit" | "Write" | "NotebookEdit")
                && !allow);
        // Aggregate guidance can import files unrelated to its requested path.
        // Until every imported file is admitted independently, read restrictions
        // apply conservatively to the entire aggregate operation.
        if matches!(name, "ProjectContext" | "Skill" | "Workflow") && self.tool == "Read" && !allow
        {
            return true;
        }
        if name == "Workflow" && self.tool == "Edit" && !allow {
            return true;
        }
        // Workflow's private registry bridge is an aggregate Agent operation.
        // Parameter-scoped restrictions also apply conservatively until each
        // generated child call goes through independent native admission.
        if name == "Workflow" && wild(self.tool, "Agent", false) && !allow {
            return true;
        }
        let mcp = self.tool.starts_with("mcp__")
            && !self.tool[5..].contains("__")
            && name.starts_with(&format!("{}__", self.tool));
        // File-deny rules cannot safely analyze arbitrary subprocess file accesses.
        // Deny shell dispatch conservatively when any file deny/ask policy applies.
        if (name == "Bash" || (name == "Monitor" && input.get("command").is_some()))
            && matches!(self.tool, "Read" | "Edit")
            && !allow
        {
            return true;
        }
        let shell_alias =
            self.tool == "Bash" && name == "Monitor" && input.get("command").is_some();
        let web_alias = self.tool == "WebFetch" && name == "Monitor" && input.get("ws").is_some();
        if !file_alias && !shell_alias && !web_alias && !mcp && !wild(self.tool, name, false) {
            return false;
        }
        let Some(spec) = self.spec else {
            return true;
        };
        if spec == "*" {
            return true;
        }
        if !allow
            && let Some((key, value)) = spec.split_once(':')
            && !matches!(key.trim(), "domain" | "skill")
            && !value.is_empty()
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            if let Some(actual) = input.get(key.trim()) {
                let actual = actual
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| actual.to_string());
                return wild(value.trim(), &actual, false);
            }
            if self.tool != "Bash" {
                return false;
            }
        }
        match self.tool {
            "Bash" => {
                let command = input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some(parts) = simple_commands(command) else {
                    return !allow;
                };
                let pattern = spec
                    .strip_suffix(":*")
                    .map(|s| format!("{s} *"))
                    .unwrap_or_else(|| spec.into());
                let matches = |part: &String| {
                    wild(&pattern, part, false)
                        || pattern.strip_suffix(" *").is_some_and(|base| base == part)
                };
                if allow {
                    parts.iter().all(matches)
                } else {
                    parts.iter().any(matches)
                }
            }
            "Read" | "Edit" => {
                let path = input
                    .get("file_path")
                    .or_else(|| input.get("notebook_path"))
                    .or_else(|| input.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or(".");
                let (anchor, pattern, anchored) = if let Some(p) = spec.strip_prefix("//") {
                    (PathBuf::from("/"), p, true)
                } else if let Some(p) = spec.strip_prefix("~/") {
                    (
                        std::env::var_os("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| cwd.to_owned()),
                        p,
                        true,
                    )
                } else if let Some(p) = spec.strip_prefix('/') {
                    (cwd.to_owned(), p, true)
                } else {
                    (
                        cwd.to_owned(),
                        spec.strip_prefix("./").unwrap_or(spec),
                        spec.starts_with("./"),
                    )
                };
                let matches = |p: &PathBuf| {
                    let Ok(rel) = p.strip_prefix(&anchor) else {
                        return false;
                    };
                    let rel = rel.to_string_lossy();
                    wild(pattern, &rel, true)
                        || (!anchored && !pattern.contains('/') && wild(&format!("**/{pattern}"), &rel, true))
                        || (!allow && !anchored && pattern.matches('/').count() == 1 && pattern.ends_with("/**") && wild(&format!("**/{pattern}"), &rel, true))
                        // Search tools consume a subtree: don't let a denied descendant leak.
                        || (!allow && matches!(name, "Glob" | "Grep" | "ProjectContext") && (rel == "." || rel.is_empty() || pattern.starts_with(&format!("{rel}/"))))
                };
                let paths = paths(cwd, path);
                if allow {
                    paths.iter().all(matches)
                } else {
                    paths.iter().any(matches)
                }
            }
            "WebFetch" => {
                let Some(domain) = spec.strip_prefix("domain:") else {
                    return false;
                };
                let Some(url) = input
                    .get("url")
                    .or_else(|| input.get("ws").and_then(|ws| ws.get("url")))
                    .and_then(Value::as_str)
                    .and_then(|s| reqwest::Url::parse(s).ok())
                else {
                    return false;
                };
                let host = url
                    .host_str()
                    .unwrap_or_default()
                    .trim_end_matches('.')
                    .to_lowercase();
                let pattern = domain.trim_end_matches('.').to_lowercase();
                if pattern == "*" {
                    true
                } else if let Some(suffix) = pattern.strip_prefix("*.") {
                    host.ends_with(&format!(".{suffix}"))
                } else {
                    wild(&pattern.replace('.', "/"), &host.replace('.', "/"), true)
                }
            }
            "Agent" => input
                .get("subagent_type")
                .and_then(Value::as_str)
                .is_some_and(|s| wild(spec, s, false)),
            "Skill" => input
                .get("skill")
                .and_then(Value::as_str)
                .is_some_and(|s| wild(spec.strip_prefix("skill:").unwrap_or(spec), s, false)),
            _ => false,
        }
    }
}
fn wild(pattern: &str, value: &str, path: bool) -> bool {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if path && chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => regex.push_str(if path { "[^/]*" } else { ".*" }),
            '?' if path => regex.push_str("[^/]"),
            other => regex.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex.push('$');
    regex::Regex::new(&regex).is_ok_and(|r| r.is_match(value))
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}
fn paths(cwd: &Path, path: &str) -> Vec<PathBuf> {
    let raw = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let lexical = normalize(&raw);
    let mut ancestor = raw.as_path();
    let mut tail = Vec::new();
    while std::fs::symlink_metadata(ancestor).is_err() {
        let Some(name) = ancestor.file_name() else {
            break;
        };
        tail.push(name.to_owned());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }
    let mut resolved = ancestor
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/__unresolved_permission_path__"));
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    vec![lexical, normalize(&resolved)]
}
/// Deliberately limited parser. Substitution, redirection, wrappers, escapes,
/// assignments, control flow and malformed separators need human review. A
/// scoped deny on Bash blocks these entirely instead of assuming no match.
fn simple_commands(command: &str) -> Option<Vec<String>> {
    if command.len() > 10000
        || command.contains(['$', '`', '\\', '>', '<', '(', ')', '{', '}', '\n', '\r'])
    {
        return None;
    }
    let mut quote = None;
    let mut part = String::new();
    let mut parts = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '\'' | '"') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
        }
        if quote.is_none() && matches!(ch, ';' | '&' | '|') {
            if part.trim().is_empty() {
                return None;
            }
            parts.push(part.trim().to_owned());
            part.clear();
            if chars.peek() == Some(&ch) && matches!(ch, '&' | '|') {
                chars.next();
            } else if ch == '&' {
                return None;
            }
        } else {
            part.push(ch);
        }
    }
    if quote.is_some() || part.trim().is_empty() {
        return None;
    }
    parts.push(part.trim().into());
    for part in &parts {
        let words = shlex::split(part)?;
        let first = words.first()?;
        if first.contains('=')
            || matches!(
                first.as_str(),
                "if" | "for"
                    | "while"
                    | "case"
                    | "until"
                    | "function"
                    | "time"
                    | "timeout"
                    | "nice"
                    | "nohup"
                    | "stdbuf"
                    | "command"
                    | "builtin"
                    | "noglob"
                    | "xargs"
                    | "watch"
                    | "setsid"
                    | "ionice"
                    | "flock"
            )
        {
            return None;
        }
        if first == "find"
            && words.iter().any(|w| {
                matches!(
                    w.as_str(),
                    "-exec" | "-execdir" | "-delete" | "-ok" | "-okdir"
                )
            })
        {
            return None;
        }
    }
    Some(parts)
}
