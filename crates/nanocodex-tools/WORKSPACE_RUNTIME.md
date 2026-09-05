# Nanocodex workspace runtime

Canonical retained workspace tools for constrained process companions.

This is the crate documentation produced with `workspace-runtime` and without
the default `native` feature. [`workspace_runtime::WorkspaceToolRuntime`]
executes `exec_command`, `write_stdin`, `apply_patch`, and `view_image` through
the same handlers used by the normal Nanocodex tools registry. Interactive
shell sessions remain alive for the lifetime of the runtime, and
[`workspace_runtime::WorkspaceToolRuntimeControl`] explicitly terminates their
process groups and descendants.

[`standard`] contains the stable tool identities and definitions. [`contract`]
and the crate-root [`Tool`], [`ToolContext`], [`ToolInput`], [`ToolOutput`], and
[`ToolResult`] types are the shared model-visible contract.

This artifact deliberately excludes the agent-facing registry, Code Mode and
QuickJS, MCP, macros, web/image generation clients, and OpenAI transport. It
exists to build small static companions such as `nanocodex-vm-guest`; it is not
a second tool implementation or an alternate mode for normal native
applications.
