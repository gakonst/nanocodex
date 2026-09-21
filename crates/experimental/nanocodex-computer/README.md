# nanocodex-computer (experimental)

This crate calls the official external Sky MCP provider. It contains no CUA
implementation, JavaScript engine, browser extension, platform control backend,
or bundled tool schema. The provider owns its tools, documentation, permissions,
and execution behavior.

`ComputerConfig::discover_or_install()` uses the upstream provisioning path on
supported hosts. `NANOCODEX_COMPUTER=/absolute/path/to/launcher` selects an explicit
MCP launcher; `off`, `none`, or `0` disables discovery. Otherwise discovery checks
only the managed upstream launcher. There is no sibling, PATH, or source-built
custom runtime fallback. Installation and operating-system permission grants
remain host operations.

```rust,ignore
use nanocodex_computer::{ComputerConfig, ComputerTools};

let config = ComputerConfig::mcp("/absolute/path/to/upstream-sky-launcher");
let computer = ComputerTools::connect(config).await?;
// Register computer.tools() in the embedding application's tool registry.
```

`ComputerConfig::new` is equivalent to `mcp`. The host can set `args` and
`environment`; the adapter appends no platform or native-control flags and does
not translate environment variables into provider arguments. Only an allowlist
of OS/session environment variables is inherited. Account credentials are not
inherited automatically.

Connection sends MCP initialization and discovers every `tools/list` page before
registration. `catalog()` retains the complete upstream definitions, including
metadata and hidden lifecycle hooks. `tools()` exposes model-visible tools;
`tool(name)` also allows trusted host code to invoke hidden hooks. Tool names use
the `mcp__cua_repl__` namespace. Descriptions, input schemas, output schemas, and
call arguments come from the provider without replacement or reinterpretation.
Every conversation process must publish the same catalog that was registered.

For another transport, implement `ComputerExecutor::invoke_tool` and construct
`ComputerTools::new(executor, discovered_catalog)`. There is no typed JavaScript
request adapter or static fallback catalog. Read the upstream tool descriptions
for supported arguments and APIs.

Each conversation owns a provider process and a queue. Calls within that process
remain ordered; other conversations execute independently. Cancellation or a
protocol failure discards only the affected process. A subsequent upstream
`js_reset` call is required before continuing that conversation.

For `js` and `js_reset`, a positive integer `timeout_ms` also bounds the full
host wait, including queueing, startup, and execution (default: 30 seconds, maximum: 2,147,483,647 ms).
Arguments still reach the provider unchanged. An active call that expires
discards its process and requires `js_reset`; a call that expires while queued
never runs or discards the active process. Other conversations remain usable.

MCP text, image, and audio content is translated to Nanocodex multimodal tool
output. The full MCP result, including structured content and metadata, remains
available as the structured result. Provider errors retain their failure status.

Nanocodex does not advertise host elicitation, display consent forms, or retain
permission decisions. Unhandled provider-to-client requests receive the standard
MCP method-not-found error; provider failures remain errors.
OpenAI's provider and operating-system permission requirements remain in effect.

Run the transport and provisioning tests without an installed provider:

```sh
cargo test -p nanocodex-computer
```

The tests use mock stdio MCP processes to verify catalog and argument fidelity,
conversation isolation, cancellation, and unsupported server requests. The ignored
`installed_external_provider_discovery_preserves_catalog_and_hides_lifecycle_hook`
smoke test uses `NANOCODEX_TEST_EXTERNAL_COMPUTER` to inspect an installed upstream
launcher. It performs discovery only and does not claim to verify native control.
