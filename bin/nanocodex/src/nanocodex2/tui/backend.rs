//! The unified TUI's backend axis.
//!
//! One driver (`run_inner`) serves both the managed account session
//! (`nanocodex`, `nc`, `nanocodex2`) and the local, non-durable agent
//! (`ncl`, `nanocodex --local`). Every backend difference that users can see is
//! decided here through [`Capabilities`], so menus, completion, help and command
//! dispatch hide or reject the same features consistently.

/// The feature set of one running backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capability flags"
)]
pub(crate) struct Capabilities {
    pub(crate) local: bool,
    pub(crate) share: bool,
    pub(crate) sites: bool,
    pub(crate) vault: bool,
    pub(crate) secure_input: bool,
    pub(crate) screen: bool,
    pub(crate) autoroute: bool,
    pub(crate) done: bool,
    pub(crate) connectors: bool,
    pub(crate) bug: bool,
    pub(crate) managed_sessions: bool,
    pub(crate) managed_btw: bool,
    pub(crate) local_btw: bool,
    pub(crate) reload: bool,
    pub(crate) handoff: bool,
    pub(crate) review_download: bool,
    pub(crate) routing: bool,
    pub(crate) voice_managed: bool,
    pub(crate) voice_realtime: bool,
    pub(crate) mcp: bool,
    pub(crate) branches: bool,
    pub(crate) collapse_split: bool,
    pub(crate) claude_host: bool,
    pub(crate) eval: bool,
    pub(crate) local_sessions: bool,
}

impl Capabilities {
    /// The account-managed TUI. Local-only features stay hidden until a
    /// managed implementation exists.
    pub(crate) const MANAGED: Self = Self {
        local: false,
        share: true,
        sites: true,
        vault: true,
        secure_input: true,
        screen: true,
        autoroute: true,
        done: true,
        connectors: true,
        bug: true,
        managed_sessions: true,
        managed_btw: true,
        local_btw: false,
        reload: true,
        handoff: true,
        review_download: true,
        routing: true,
        voice_managed: true,
        voice_realtime: false,
        mcp: false,
        branches: false,
        collapse_split: false,
        claude_host: false,
        eval: false,
        local_sessions: false,
    };

    /// The local agent TUI before its optional runtime pieces are known.
    pub(crate) const LOCAL: Self = Self {
        local: true,
        share: false,
        sites: false,
        vault: false,
        secure_input: false,
        screen: false,
        autoroute: false,
        done: false,
        connectors: false,
        bug: false,
        managed_sessions: false,
        managed_btw: false,
        local_btw: true,
        reload: false,
        handoff: false,
        review_download: false,
        routing: false,
        voice_managed: false,
        voice_realtime: true,
        mcp: true,
        branches: true,
        collapse_split: true,
        claude_host: true,
        eval: cfg!(any(
            all(target_os = "linux", not(target_env = "musl")),
            all(target_os = "macos", target_arch = "aarch64")
        )),
        local_sessions: true,
    };

    /// Shared policy for typed commands, action search and shortcut help.
    pub(crate) fn command_available(self, input: &str) -> bool {
        let command = input
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_start_matches('/');
        match command {
            "share" => self.share,
            "sites" => self.sites,
            "vault" => self.vault,
            "secure-input" => self.secure_input,
            "screen" => self.screen,
            "autoroute" => self.autoroute,
            "done" | "undone" => self.done,
            "connect" | "connectors" => self.connectors,
            "bug" => self.bug,
            "reload" => self.reload,
            "handoff" => self.handoff,
            "mcp" => self.mcp,
            "branches" => self.branches,
            "collapse" | "split" => self.collapse_split,
            "benchmark" => self.eval,
            "voice" => self.voice_managed || self.voice_realtime,
            "btw" | "close" => self.managed_btw || self.local_btw,
            "attach" | "resume" => self.managed_sessions || self.local_sessions,
            _ => true,
        }
    }

    /// The error shown when a hidden command is typed anyway.
    pub(crate) fn unavailable(self, what: &str) -> String {
        if self.local {
            format!("{what} needs a Nanocodex account session (run nanocodex)")
        } else {
            format!("{what} is available only in the local agent (run ncl)")
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::MANAGED
    }
}
