//! Restore a mobile working set as attached terminals, without submitting any turns.
use clap::Args;
use nanocodex_managed::{AgentList, ManagedClient, ManagedError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::IsTerminal,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Args)]
pub(super) struct Options {
    /// Include sessions with a user message in the last N hours, plus all running sessions.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u64).range(1..=8760))]
    hours: u64,
    /// Target tmux session (default: current session inside tmux, otherwise nanocodex).
    #[arg(long, value_parser = safe_name)]
    session: Option<String>,
    /// Use an isolated tmux server named NAME.
    #[arg(long, value_parser = safe_name)]
    tmux_socket: Option<String>,
    /// Create/reuse windows without attaching or switching the terminal.
    #[arg(long, conflicts_with = "dry_run")]
    detach: bool,
    /// Print the selected sessions as JSON without creating any windows.
    #[arg(long)]
    dry_run: bool,
}

fn safe_name(value: &str) -> Result<String, String> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        Ok(value.to_owned())
    } else {
        Err("use 1-128 letters, digits, underscores or hyphens".into())
    }
}

#[derive(Serialize)]
struct Session {
    agent_id: String,
    title: String,
    window_name: String,
    status: String,
    last_user_message_at: u64,
}

fn timestamp(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (if value < 10_000_000_000.0 {
        value * 1000.0
    } else {
        value
    }) as u64
}

fn selected(list: &AgentList, hours: u64) -> Vec<Session> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now.saturating_sub(hours * 3_600_000);
    let mut seen = HashSet::new();
    let mut sessions = Vec::new();
    for id in &list.data {
        if !super::valid_managed_agent_id(id) || !seen.insert(id) {
            continue;
        }
        let Some(summary) = list.summaries.get(id) else {
            continue;
        };
        let p = summary.presentation.as_ref();
        if p.and_then(|p| p.get("done")).and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let status = p
            .and_then(|p| p.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let running = status == "running"
            || p.and_then(|p| p.get("activeTurnIds"))
                .and_then(|v| v.as_array())
                .is_some_and(|ids| !ids.is_empty());
        // A known zero means no user message, not a reason to use background activity.
        let touched = match p.and_then(|p| p.get("lastUserMessageAt")) {
            Some(value) => value.as_f64().map(timestamp).unwrap_or(0),
            None if summary.turn_count > 0 => timestamp(summary.updated_at.max(summary.created_at)),
            None => 0,
        };
        if !running && (touched == 0 || touched < cutoff) {
            continue;
        }
        let prompt = p
            .and_then(|p| p.get("lastUserPrompt"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title = clean_text(if summary.title.trim().is_empty() {
            prompt
        } else {
            &summary.title
        });
        let title = if title.is_empty() { id.clone() } else { title };
        sessions.push(Session {
            agent_id: id.clone(),
            window_name: title.graphemes(true).take(15).collect(),
            title,
            status: status.to_owned(),
            last_user_message_at: touched,
        });
    }
    sessions.sort_by(|a, b| {
        let active = |s: &Session| s.status == "running";
        active(b)
            .cmp(&active(a))
            .then(b.last_user_message_at.cmp(&a.last_user_message_at))
            .then(a.agent_id.cmp(&b.agent_id))
    });
    sessions
}

fn clean_text(value: &str) -> String {
    // Strip terminal controls before naming windows or printing remote strings.
    // Splitting whitespace collapses newlines/tabs rather than joining words.
    value
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn failure(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

struct Tmux {
    socket: Option<String>,
}
impl Tmux {
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        // A newly created tmux server retains its launch environment. Keep the
        // selected account credentials only in the private per-pane IPC handoff.
        command
            .env_remove("NANOCODEX_API_KEY")
            .env_remove("NC_API_KEY");
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command.kill_on_drop(true).stdin(Stdio::null());
        command
    }
    async fn output(&self, args: &[&str]) -> Result<std::process::Output, ManagedError> {
        tokio::time::timeout(Duration::from_secs(5), self.command().args(args).output())
            .await
            .map_err(|_| failure("tmux did not respond within five seconds"))?
            .map_err(|e| failure(format!("could not run tmux (install tmux first): {e}")))
    }
    async fn checked(&self, args: &[&str]) -> Result<String, ManagedError> {
        let output = self.output(args).await?;
        if !output.status.success() {
            return Err(failure(format!(
                "tmux failed: {}",
                clean_text(&String::from_utf8_lossy(&output.stderr))
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    }
    async fn panes(&self, target: &str) -> Result<Vec<Pane>, ManagedError> {
        let output = self.checked(&["list-panes", "-s", "-t", target, "-F", "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_current_command}|#{@nanocodex-continue-agent}|#{@nanocodex-overview}"]).await?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(6, '|');
                let pane = fields.next()?.to_owned();
                let window = fields.next()?.to_owned();
                let dead = fields.next()? == "1";
                let current_command = fields.next().unwrap_or_default();
                let marker = fields.next().unwrap_or_default();
                let overview: Option<serde_json::Value> =
                    serde_json::from_str(fields.next().unwrap_or_default()).ok();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let fresh = overview
                    .as_ref()
                    .and_then(|v| v.get("updated_at"))
                    .and_then(|v| v.as_u64())
                    .is_some_and(|at| at >= now.saturating_sub(10_000));
                let agent = if dead {
                    marker
                } else if !matches!(current_command, "nanocodex" | "nanocodex2" | "nc") {
                    ""
                } else if fresh {
                    // A fresh empty identity means /new/connecting, not the previous thread.
                    overview
                        .as_ref()
                        .and_then(|v| v.get("agent_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                } else {
                    marker
                };
                let agent = if super::valid_managed_agent_id(agent) {
                    agent.to_owned()
                } else {
                    String::new()
                };
                Some(Pane {
                    pane,
                    window,
                    dead,
                    agent,
                })
            })
            .collect())
    }
}
struct Pane {
    pane: String,
    window: String,
    dead: bool,
    agent: String,
}

// Serialize restoration locally so concurrent invocations do not create duplicates.
// The lock is process-owned and released even after errors; no tmux wait-for lock can linger.
async fn restore_lock(socket: Option<&str>) -> Result<File, ManagedError> {
    let scope = format!(
        "{}:{:?}:{:?}:{}",
        whoami::username(),
        std::env::var_os("HOME"),
        std::env::var_os("TMUX_TMPDIR"),
        socket.unwrap_or("default")
    );
    let hash = hex::encode(Sha256::digest(scope));
    let path = std::env::temp_dir().join(format!("nanocodex-continue-{hash}.lock"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| failure(format!("could not lock session restore: {e}")))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => return Err(failure(format!("another session restore is busy: {e}"))),
        }
    }
}

pub(super) async fn run(client: &ManagedClient, options: Options) -> Result<(), ManagedError> {
    let sessions = selected(&client.list().await?, options.hours);
    if options.dry_run {
        return super::write_json(&sessions);
    }
    if sessions.is_empty() {
        eprintln!(
            "No unfinished sessions from the last {} hours or still running.",
            options.hours
        );
        return Ok(());
    }
    let inside = std::env::var_os("TMUX").is_some() && options.tmux_socket.is_none();
    if !options.detach && !std::io::stdin().is_terminal() {
        return Err(failure(
            "continue needs an interactive terminal; use --detach to create windows or --dry-run to preview",
        ));
    }
    let tmux = Tmux {
        socket: options.tmux_socket.clone(),
    };
    tmux.checked(&["-V"]).await?;
    let _lock = restore_lock(options.tmux_socket.as_deref()).await?;
    let name = options.session.as_deref().unwrap_or("nanocodex");
    let mut target = if inside && options.session.is_none() {
        let pane = std::env::var("TMUX_PANE")
            .map_err(|_| failure("TMUX_PANE is missing; select --session NAME"))?;
        Some(
            tmux.checked(&["display-message", "-p", "-t", &pane, "#{session_id}"])
                .await?,
        )
    } else if tmux
        .output(&["has-session", "-t", &format!("={name}")])
        .await?
        .status
        .success()
    {
        Some(
            tmux.checked(&[
                "display-message",
                "-p",
                "-t",
                &format!("={name}"),
                "#{session_id}",
            ])
            .await?,
        )
    } else {
        None
    };
    let mut panes = match &target {
        Some(target) => tmux.panes(target).await?,
        None => Vec::new(),
    };
    let exe = std::env::current_exe().map_err(|e| failure(e.to_string()))?;
    let cwd = std::env::current_dir().map_err(|e| failure(e.to_string()))?;
    let exe = exe
        .to_str()
        .ok_or_else(|| failure("CLI executable path must be UTF-8"))?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| failure("working directory must be UTF-8"))?;
    // Give new child processes the caller's exact account over private local IPC,
    // never through tmux arguments/environment or a credential file on disk.
    let expected = sessions
        .iter()
        .filter(|s| !panes.iter().any(|p| p.agent == s.agent_id && !p.dead))
        .map(|s| s.agent_id.clone())
        .collect::<Vec<_>>();
    let handoff = if expected.is_empty() {
        None
    } else {
        Some(super::continue_auth::Handoff::start(expected).await?)
    };
    let quote = |v: &str| {
        shlex::try_quote(v)
            .map(|v| v.into_owned())
            .map_err(|_| failure("command argument contains a NUL byte"))
    };
    let config_env = [
        "CODEX_HOME",
        "NANOCODEX_HOME",
        "NANOCODEX_ACCOUNT_FILE",
        "NANOCODEX_DISABLE_HAND",
        "NANOCODEX_COMPUTER",
        "NANOCODEX_RELOAD_DIR",
        "SSH_TTY",
    ];
    let mut launch_env = String::from("env -u NANOCODEX_API_KEY -u NC_API_KEY");
    let mut assignments = String::new();
    for key in config_env {
        if let Some(value) = std::env::var_os(key) {
            let value = value
                .to_str()
                .ok_or_else(|| failure(format!("{key} must be UTF-8")))?;
            assignments.push(' ');
            assignments.push_str(&quote(&format!("{key}={value}"))?);
        } else {
            launch_env.push_str(&format!(" -u {key}"));
        }
    }
    launch_env.push_str(&assignments);
    let mut first_window = None;
    let mut opened = 0;
    for session in sessions {
        let launch = if let Some(handoff) = &handoff {
            let socket = handoff
                .path()
                .to_str()
                .ok_or_else(|| failure("private handoff path must be UTF-8"))?;
            format!(
                "exec {launch_env} {} __continue-attach {} {}",
                quote(exe)?,
                quote(socket)?,
                quote(&session.agent_id)?
            )
        } else {
            String::new()
        };
        let existing = panes.iter().find(|p| p.agent == session.agent_id);
        let (window, pane, created) = if let Some(existing) = existing {
            if existing.dead {
                tmux.checked(&[
                    "respawn-pane",
                    "-k",
                    "-t",
                    &existing.pane,
                    "-c",
                    cwd,
                    &launch,
                ])
                .await?;
            }
            (existing.window.clone(), existing.pane.clone(), false)
        } else {
            let output = if let Some(target) = &target {
                tmux.checked(&[
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id}|#{window_id}|#{pane_id}",
                    "-t",
                    target,
                    "-n",
                    &session.window_name,
                    "-c",
                    cwd,
                    &launch,
                ])
                .await?
            } else {
                tmux.checked(&[
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id}|#{window_id}|#{pane_id}",
                    "-s",
                    name,
                    "-n",
                    &session.window_name,
                    "-c",
                    cwd,
                    &launch,
                ])
                .await?
            };
            let fields: Vec<_> = output.split('|').collect();
            if fields.len() != 3 {
                return Err(failure(
                    "tmux returned an unexpected new-window receipt; rerun continue to recover",
                ));
            }
            target = Some(fields[0].to_owned());
            let window = fields[1].to_owned();
            let pane = fields[2].to_owned();
            panes.push(Pane {
                window: window.clone(),
                pane: pane.clone(),
                dead: false,
                agent: session.agent_id.clone(),
            });
            (window, pane, true)
        };
        // Pane identity, not truncated window names, determines deduplication.
        tmux.checked(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@nanocodex-continue-agent",
            &session.agent_id,
        ])
        .await?;
        tmux.checked(&["set-option", "-w", "-t", &window, "automatic-rename", "off"])
            .await?;
        tmux.checked(&["set-option", "-w", "-t", &window, "allow-rename", "off"])
            .await?;
        tmux.checked(&["rename-window", "-t", &window, "--", &session.window_name])
            .await?;
        eprintln!(
            "{} {} · {}",
            if created { "Opened" } else { "Reused" },
            session.window_name,
            session.agent_id
        );
        if first_window.is_none() {
            first_window = Some(window);
        }
        opened += 1;
    }
    if let Some(handoff) = handoff {
        handoff.finish().await?;
    }
    let target = target.expect("selected sessions created a target");
    eprintln!("{opened} session(s) ready in tmux.");
    drop(_lock);
    if options.detach {
        return Ok(());
    }
    if let Some(window) = first_window {
        tmux.checked(&["select-window", "-t", &window]).await?;
    }
    let mut command = tmux.command();
    if inside {
        command.args(["switch-client", "-t", &target]);
        // Bind the switching client to the invoking pane instead of another user's
        // most recently active client on a shared tmux server.
        if let Ok(pane) = std::env::var("TMUX_PANE") {
            let client = tmux
                .checked(&["display-message", "-p", "-t", &pane, "#{client_name}"])
                .await?;
            if !client.is_empty() {
                command.args(["-c", &client]);
            }
        }
    } else {
        command.args(["attach-session", "-t", &target]);
    }
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map_err(|e| failure(format!("could not attach tmux: {e}")))?;
    if !status.success() {
        return Err(failure(
            "tmux attach failed; windows remain available for another continue",
        ));
    }
    Ok(())
}
