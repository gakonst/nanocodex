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
The Windows managed receipt supplies its exact arguments and environment.

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
including its own optional fields and timeouts. Tool execution deadlines and
reset behavior belong to the provider.

Each conversation has its own process and ordered call queue. Independent
conversations run concurrently. Caller cancellation stops the active process;
queued cancellation rejects without running that call. Session release and
attachment close cancel their active and queued work. A later call after a
transport failure starts a fresh provider process; the adapter does not require
an invented reset command. It never retries a failed call automatically.

MCP results and metadata remain available unchanged as the tool result's `value`.
Text, images, and audio are translated into model content; other MCP content is
represented as its JSON text. Images retain the provider's declared MIME type
and use original detail. The adapter does not reinterpret screenshot bytes.

CUA calls carry `session_id`, `thread_id`, `call_id`, and `model` in
`x-codex-turn-metadata`, plus `turn_id` when supplied by the agent runtime. The
adapter never derives a turn ID from a tool call ID.

Hosts with a genuine user-facing form UI can provide `elicitationHandler`:

```js
const computer = await connectComputerTools({
  executable: providerExecutable,
  args: providerArgs,
  elicitationHandler: async (params, context) => {
    // Display the provider's message, schema, and metadata in a real host UI.
    // Dismiss the form when context.signal is aborted.
    return await showHostForm(params, context);
  },
});
```

Only a configured handler advertises MCP `elicitation.form`. Form requests from
`elicitation/create` or `openai/elicitation/create` are forwarded with their raw
parameters and `_meta`; omitted mode means form. The host response preserves its
content and metadata. The adapter never fabricates acceptance or persistence.
Without a handler it returns method-not-found. Unsupported URL requests return
an error. The desktop app currently supplies no form callback and therefore does
not advertise interactive elicitation.

Context includes `requestId`, `signal`, and the active tool's `sessionId`,
`callId`, and `model`; discovery-time requests have no active tool identity.
`elicitationTimeoutMs` defaults to 300000 and must be a positive safe integer.
Expiry or provider cancellation returns `cancel`. Caller abort, call completion,
release, process exit, and attachment close dismiss pending forms; late responses
are ignored. Host exceptions produce an internal error without exposing details.

Run `pnpm --filter nanocodex-computer test` and
`pnpm --filter nanocodex-computer typecheck`. Tests use synthetic MCP protocol
fixtures; they do not ship a replacement CUA runtime. See
[provider installation](../../docs/computer/upstream-provider.md).
