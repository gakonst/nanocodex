/// How the transcript shows tool calls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum ToolCalls {
    /// Each call with its arguments, output, and patch.
    #[default]
    Expanded,
    /// One summary line per call.
    Folded,
    /// No tool rows; the footer still shows the turn as Working.
    Hidden,
}
