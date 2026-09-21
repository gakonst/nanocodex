# nanocodex-computer

MCP attachment for the official OpenAI CUA provider (Sky on macOS). The installed
provider owns its tools, descriptions, schemas, JavaScript API, and permissions.
This package contains no CUA implementation, browser API facade, or fallback
runtime. It launches the provider with its exact trusted command and arguments.

```js
import { connectComputerTools, ensureComputer } from "nanocodex-computer";

const executable = await ensureComputer({ binary: installedNativeHelper });
if (!executable) throw new Error("No official CUA provider is configured");
const computer = await connectComputerTools({ executable });
// Add computer.tools to createTools({ tools, workspace }).
// Read the discovered provider declarations before using any CUA API.
await computer.close();
```

`ensureComputer({ binary })` uses the installed `nanocodex2` or `nanocodex` helper
to provision the official provider on macOS and Windows. `discoverComputer` is
read-only. Both honor `NANOCODEX_COMPUTER` as an explicit provider executable,
and `off`, `none`, or `0` disable CUA. Neither searches for the retired companion
in PATH, Cargo directories, source builds, or adjacent installations. Unsupported
platforms return no provider unless an explicit MCP executable is configured.
The managed receipt supplies its exact arguments and environment on both platforms.
Mac setup selects immutable host assets separately from the signed bundle. CUA
then starts the official app server without launching the desktop GUI; see the
[managed Mac host](../../docs/computer/official-app-server-bridge.md).

`connectComputerTools` discovers the full paginated MCP catalog before exposing
an attachment. `definitions` and each tool's `providerDefinition` preserve the
provider declarations, including optional metadata. The tools are published as
`mcp__cua_repl__<provider name>`. Entries whose `_meta.ui.visibility` excludes
`model` remain available through trusted `tool(name)` lookup. Each conversation
process must present the same catalog. `createComputerTools` is a synchronous
constructor for hosts that already have that trusted discovered catalog; it
never invents one. The `/contract` export contains only namespace name constants.

Trusted `args` and `environment` configure the child process. There are no
companion launch flags, platform arguments, security configuration, private
desktop routing, or protocol switches. The inherited child environment omits
account/API credentials. The provider receives model arguments unchanged,
including its own optional fields and timeouts. For `js` and `js_reset`, the host
also enforces a deadline covering queue wait, process startup, and the provider
call. A positive safe integer `timeout_ms` selects that deadline (clamped to
2,147,483,647 ms to avoid timer overflow); all other values default to 30 seconds.
The provider still owns reset behavior.

Each conversation has its own process and ordered call queue. Independent
conversations run concurrently. Caller cancellation stops the active process;
queued cancellation rejects without running that call. Host deadline expiry uses
the same cancellation path: active calls stop their process, while queued calls
never run or stop another call's process. Session release and attachment close
cancel their active and queued work. A later call after a transport failure or
active deadline expiry starts a fresh provider process; the adapter does not
require an invented reset command. It never retries a failed call automatically.

MCP results and metadata remain available unchanged as the tool result's `value`.
Text, images, and audio are translated into model content; other MCP content is
represented as its JSON text. Images retain the provider's declared MIME type
and use original detail. The adapter does not reinterpret screenshot bytes.

CUA calls carry `session_id`, `thread_id`, `call_id`, and `model` in
`x-codex-turn-metadata`, plus `turn_id` when supplied by the agent runtime. The
adapter never derives a turn ID from a tool call ID.

Permissions and consent belong to the official OpenAI provider. The attachment
advertises no MCP client capabilities and responds to incoming provider RPC
requests with standard method-not-found (`-32601`) errors. Provider notifications
receive no response.

Run `pnpm --filter nanocodex-computer test` and
`pnpm --filter nanocodex-computer typecheck`. Tests use synthetic MCP protocol
fixtures; they do not ship a replacement CUA runtime. See
[provider installation](../../docs/computer/upstream-provider.md).
