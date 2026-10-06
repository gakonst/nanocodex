#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) usage: &'static str,
    pub(super) description: &'static str,
    pub(super) accepts_arguments: bool,
    requires_btw: bool,
}

const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        usage: "/model [model]",
        description: "Choose the model for a new thread",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/thinking",
        usage: "/thinking [effort]",
        description: "Choose the reasoning effort",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/fast",
        usage: "/fast [on|off]",
        description: "Toggle priority processing",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/btw",
        usage: "/btw [question]",
        description: "Open a side exploration",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/simplify",
        usage: "/simplify [focus]",
        description: "Review and simplify the current work",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/voice",
        usage: "/voice [voice|on|off|mute|list]",
        description: "Control the voice session",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/mcp login",
        usage: "/mcp login <server>",
        description: "Authenticate with an MCP server",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/mcp reload",
        usage: "/mcp reload <server>",
        description: "Reload an MCP server",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/cancel",
        usage: "/cancel",
        description: "Cancel the active turn",
        accepts_arguments: false,
        requires_btw: false,
    },
    SlashCommand {
        name: "/trace",
        usage: "/trace",
        description: "Open this session's traces",
        accepts_arguments: false,
        requires_btw: false,
    },
    SlashCommand {
        name: "/benchmark",
        usage: "/benchmark [profile]",
        description: "Run the benchmark workflow",
        accepts_arguments: true,
        requires_btw: false,
    },
    SlashCommand {
        name: "/collapse",
        usage: "/collapse",
        description: "Merge this BTW into the main thread",
        accepts_arguments: false,
        requires_btw: true,
    },
    SlashCommand {
        name: "/split",
        usage: "/split",
        description: "Detach this BTW into another terminal",
        accepts_arguments: false,
        requires_btw: true,
    },
    SlashCommand {
        name: "/close",
        usage: "/close",
        description: "Dismiss this BTW thread",
        accepts_arguments: false,
        requires_btw: true,
    },
];

pub(super) fn matching(input: &str, cursor: usize, has_btw: bool) -> Vec<&'static SlashCommand> {
    if cursor != input.len() || !input.starts_with('/') || input.contains('\n') {
        return Vec::new();
    }
    let query = input.to_ascii_lowercase();
    COMMANDS
        .iter()
        .filter(|command| (!command.requires_btw || has_btw) && command.name.starts_with(&query))
        .collect()
}
