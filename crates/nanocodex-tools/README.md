# Nanocodex Tools

Tool building blocks for `OpenAI` agents.

`nanocodex-tools` is useful without the Nanocodex agent loop. It provides the
caller-defined [`Tool`] contract, `tool` macro, heterogeneous [`Tools`]
registry, Code Mode runtime, standard workspace tools, and native MCP clients.
The shared contract types are defined by `nanocodex-oai-api` and re-exported
here so a tool implementation has one import surface.

## Define and select tools

The definition is the single source of truth for a tool's registry name. The
macro derives its input and output schemas from the function:

```rust
use nanocodex_tools::{Tools, tool};

#[tool(
    name = "deployment_region",
    description = "Return the production region for a named service.",
    parallel = true
)]
async fn deployment_region(service: String) -> Result<String, std::io::Error> {
    Ok(format!("{service}: us-west-2"))
}

# fn build() -> Result<(), nanocodex_tools::ToolsBuildError> {
let tools = Tools::builder()
    .without_defaults()
    .tool(deployment_region)
    .build()?;
# Ok(())
# }
```

`Tools` defaults to `ToolExposure::CodeModeOnly`, where ordinary tools are
available through `exec` and only Code Mode entrypoints are directly visible.
Select `ToolExposure::DirectAndCodeMode` when a consumer needs the same
ordinary tools directly as well as through `exec`:

```rust
use nanocodex_tools::{ToolExposure, Tools};

# fn build() -> Result<(), nanocodex_tools::ToolsBuildError> {
let tools = Tools::builder()
    .exposure(ToolExposure::DirectAndCodeMode)
    .build()?;
# Ok(())
# }
```

Matching Codex, direct-plus-Code-Mode exposure keeps `exec` terse and
adds each typed `exec` declaration to the corresponding direct tool; Code
Mode-only instead carries the complete nested catalog in `exec`. Selection
changes model-visible exposure, not registration or dispatch behavior.
`tool_with_exposure` can override one registered tool with `DirectOnly`,
`CodeModeOnly`, `DirectAndCodeMode`, or `Hidden` while preserving the global
default for the rest. Host-owned `exec`, `wait`, and `tool_search` names cannot
be replaced, and colliding normalized JavaScript names are rejected when the
recipe is built.

`ToolsBuilder::add` composes the same recipe from a fixed [`Tool`], a
[`WorkspaceTools`] value, or [`Mcp`](mcp::Mcp). A workspace source selects canonical
local workspace tools rooted at its directory; adding a second workspace source
is rejected.
Namespaced Code Mode names such as `image_gen__imagegen` remain available to
`exec`; normal Code Mode exposes the Codex-compatible `image_gen.imagegen`
Responses namespace and routes its namespaced call to the same handler.

Macro tools execute serially unless `parallel = true` explicitly marks their
local effects as safe to overlap. This does not change the provider wire
protocol.

Implement [`Tool`] directly when execution needs [`ToolContext`], freeform
input, multimodal [`ToolOutput`], or a custom definition:

```rust
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult,
    contract::async_trait,
};
use serde_json::json;

struct DeploymentRegion;

#[async_trait]
impl Tool for DeploymentRegion {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "deployment_region",
            "Return the production region for a named service.",
            json!({
                "type": "object",
                "properties": { "service": { "type": "string" } },
                "required": ["service"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        input: ToolInput,
        _context: ToolContext<'_>,
    ) -> ToolResult {
        let input: serde_json::Value = input.decode_json()?;
        let service = input["service"].as_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "service must be a string",
            )
        })?;
        Ok(ToolOutput::text(format!("{service}: us-west-2")))
    }
}
```

## Embed Code Mode in another host

[`embedded`] is the portable boundary for environments that own JavaScript
execution outside Rust. Build the ordinary [`Tools`] recipe, implement
[`embedded::CodeModeHost`], and bind that host at the embedding boundary. The
adapter reuses the same execution, nested-call, notification, observer, and
owned-context types as native Code Mode. The [`embedded`] module documentation
includes a complete host implementation.

## MCP is native and always available

MCP is not a feature flag. Native consumers configure stdio or Streamable HTTP
servers and install the provider into the same registry:

```rust
use nanocodex_tools::{
    Tools,
    mcp::{Mcp, McpServer},
};

# fn build() -> Result<(), Box<dyn std::error::Error>> {
let mcp = Mcp::builder()
    .server(
        "company_docs",
        McpServer::stdio("company-docs-mcp").arg("--readonly"),
    )
    .build()?;

let tools = Tools::builder().add(mcp).build()?;
# Ok(())
# }
```

Handshakes and discovery start with the owning runtime. Both exposure policies
keep the provider-native `tool_search` visible while omitting deferred MCP
schemas from the initial request. Code Mode lists those deferred tools as
compact name/description entries in `ALL_TOOLS`. Search results contain loadable
MCP namespaces for direct model calls and also activate matching Code Mode
definitions, keeping large catalogs out of the initial tool list.
`McpServer::tool_exposure` independently selects `DeferredOnly`,
`CodeModeOnly`, `DeferredAndCodeMode`, or `Hidden` for each server. Automatic
catalog pagination is bounded by page, item, cursor, and wall-clock limits.

## Attach one immutable recipe

With the `attachment` feature, the same `Tools` value can execute behind a
reverse WebSocket. Attachment consumes the complete recipe, finishes MCP
discovery, validates the immutable catalog, connects, and waits for the remote
catalog acknowledgement:

```rust,no_run
use nanocodex_tools::{Tools, attachment::AttachmentTarget};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let tools = Tools::builder().without_defaults().build()?;
let target = AttachmentTarget::new("wss://tools.example.test/v1/attach", "bearer")?;
let (attachment, mut events) = tools.attach(target).connect().await?;

let observer = tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        println!("{event:?}");
    }
});
attachment.detach().await?;
observer.await?;
# Ok(())
# }
```

Only sources with a concrete attached executor are accepted: fixed tools,
MCP added with `add`, and pinned `WorkspaceTools`. Generic dynamic providers
remain local-only. Built-in web search and image generation also remain at the
model backend, so attached recipes disable defaults and opt into concrete
sources. Preparation and discovery errors happen before a socket is opened;
`connect` returns only after readiness. The cheap `Attachment` control handle
is cloneable and its last drop detaches. `AttachmentEvents` is an independent,
bounded, best-effort observer: lag may drop events but can never delay tool
execution or protocol progress. `Attachment::status` and `closed` are the
authoritative lifecycle APIs.

### Deferred: workspace replication

Attachment chooses where a tool call executes; it does not move or merge
workspace data. An attached local workspace and the detached cloud
`/workspace` may therefore diverge. Automatic replication and reconciliation
are intentionally deferred to a separate capability with explicit direction,
conflict, and recovery policy. Until that capability exists, attach and detach
must never imply synchronization.

## Companion workspace runtimes

The default `native` feature remains the complete tools crate: registry, Code
Mode, MCP, web/image tools, macros, and standard workspace tools.

The narrower `workspace-runtime` feature exists only for process companions
such as `nanocodex-vm-guest`. With default features disabled, it exposes the
canonical [`workspace_runtime::WorkspaceToolRuntime`], standard tool
identities, and their shared contracts without linking OpenAI transports, Code
Mode/QuickJS, MCP, or HTTP clients. This is artifact separation, not a second
tool implementation or an alternate mode for normal native applications.

## Going lower level

The crate root intentionally contains only the normal registry path:
[`Tools`], `ToolsBuilder`, `ToolsBuildError`, [`Tool`], `tool`, and the types
required by the `Tool` methods. [`ToolExposure`] is the advanced declaration
policy for consumers that compare direct and Code Mode calls. The root also exposes
[`ambient_sensitive_environment`](crate::ambient_sensitive_environment) for
deliberately restoring proxy-safe credential markers to tool subprocesses.

- [`contract`] contains complete model-visible inputs, outputs, errors, and
  retained wire forms.
- [`embedded`] contains the portable application-owned Code Mode boundary.
- [`runtime`] contains the stateful per-agent executor, built-in connection
  configuration, and dynamic-provider contract.
- [`code_mode`] contains cell results, notifications, and nested-tool updates.
- `mcp` contains transport configuration, authentication, discovery, login,
  and runtime control.
- `standard` contains reusable standard-tool identities and the host-owned
  plan implementation.
- [`workspace_runtime`] contains the retained canonical workspace-tool runtime
  used by process companions.
- [`image`] contains prompt-image preparation and tool-output normalization.
