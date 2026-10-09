//! /mcp login and /mcp reload for the local agent's MCP servers.
//!
//! Port of the legacy worker's mcp_login/mcp_reload: login opens the server's
//! OAuth page in the browser, waits for the callback and reconnects; reload
//! reconnects the server and reports its tool count. Both run off the input
//! loop and report through [FeatureHost].

use nanocodex::tools::mcp::McpHandle;
use tokio::task::JoinSet;

use super::{Feature, FeatureCommand, FeatureContext, FeatureHost};
use crate::nanocodex2::tui::{local::agent::LocalParts, pane::PaneId};

#[derive(Default)]
pub(crate) struct Mcp {
    handle: Option<McpHandle>,
    tasks: JoinSet<()>,
}

impl Feature for Mcp {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn attach(&mut self, parts: &mut LocalParts, _cx: &FeatureContext<'_>) {
        self.shutdown();
        self.handle = parts.mcp.clone();
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        let (name, login) = match command {
            FeatureCommand::McpLogin(name) => (name.clone(), true),
            FeatureCommand::McpReload(name) => match name {
                Some(name) => (name.clone(), false),
                None => {
                    cx.host.error(Some(pane), "Usage: /mcp reload <server>");
                    return true;
                }
            },
            _ => return false,
        };
        let Some(handle) = self.handle.clone() else {
            cx.host.error(
                Some(pane),
                format!("MCP server {name}: MCP is not configured"),
            );
            return true;
        };
        let host = cx.host.clone();
        if login {
            self.tasks.spawn(login_server(handle, name, pane, host));
        } else {
            self.tasks.spawn(reload_server(handle, name, pane, host));
        }
        true
    }

    fn shutdown(&mut self) {
        self.tasks.abort_all();
        self.handle = None;
    }
}

async fn login_server(handle: McpHandle, name: String, pane: PaneId, host: FeatureHost) {
    let login = match handle.login(&name).await {
        Ok(login) => login,
        Err(error) => return host.error(Some(pane), format!("MCP server {name}: {error}")),
    };
    let url = login.authorization_url().to_owned();
    if let Err(error) = open_browser(&url) {
        return host.error(
            Some(pane),
            format!("MCP server {name}: failed to open OAuth page: {error}; open {url} manually"),
        );
    }
    host.notice(
        Some(pane),
        format!("Authorizing MCP server {name} in browser"),
    );
    match login.wait().await {
        Ok(tool_count) => host.notice(
            Some(pane),
            format!("Authenticated and reloaded MCP server {name} ({tool_count} tools)"),
        ),
        Err(error) => host.error(Some(pane), format!("MCP server {name}: {error}")),
    }
}

async fn reload_server(handle: McpHandle, name: String, pane: PaneId, host: FeatureHost) {
    match handle.reload(&name).await {
        Ok(tool_count) => host.notice(
            Some(pane),
            format!("Reloaded MCP server {name} ({tool_count} tools)"),
        ),
        Err(error) => host.error(Some(pane), format!("MCP server {name}: {error}")),
    }
}

/// Opens the OAuth page with the platform launcher (legacy browser_command).
fn open_browser(url: &str) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}
