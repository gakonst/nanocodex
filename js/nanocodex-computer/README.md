# nanocodex-computer (experimental)

Node attachment adapter for the persistent Nanocodex CUA companion. It exports
the `mcp__cua_repl__js` / `mcp__cua_repl__js_reset` names used by
Codex MCP integration. The installed provider owns the descriptions and API.

```js
import { connectComputerTools, discoverComputer } from "nanocodex-computer";

const executable = await discoverComputer();
if (!executable) throw new Error("Install the companion with pnpm install:computer");
const computer = await connectComputerTools({ executable });
// Add computer.tools to your existing createTools({ tools, workspace }) call.
// Close the attachment before completing its resource cleanup:
await computer.close();
```

Each conversation has its own persistent process. Calls are serialized, and
cancellation stops the process before another call can continue. A failed
transport requires `cua_repl.js_reset`. The named tools implement `releaseSession` and
`dispose` for attachment cleanup. Releasing a conversation cancels its active and
queued calls; a new conversation with the same ID receives a fresh scope.
Screenshot results are API image content with original detail, including when
consumed through Code Mode. MCP result metadata and current Codex call metadata
survive the adapter. Optional `title`/`timeout_ms` accept `null`; omitting the timeout
does not create an artificial deadline, and positive safe-integer deadlines are
supported without Node's 32-bit timer overflow.

`createComputerTools` accepts trusted `args`, `environment` and a Linux Hand's
private `desktopRuntime` directory. Those are host configuration, not model
arguments. The child environment omits account/API credentials.

`nanocodex-computer/contract` exports the tool descriptions, JSON schemas and
input validator without importing Node APIs, for hosted Workers and brokers.

See the [runtime and integration documentation](../../crates/experimental/nanocodex-computer/README.md)
for OS requirements, CDP setup, release packaging and validation commands.

`connectComputerTools` discovers MCP descriptions and schemas before publishing
an attachment, then checks each conversation process against that catalog.
Use `transport: "mcp"` with an external provider's exact executable and args;
companion-specific flags are not added in that mode. Every discovered tool is
routed with its provider-owned schema and arguments, without imposing the bundled
companion contract. Optional MCP metadata is preserved in `definitions` and each
tool's `providerDefinition`. Tools whose `_meta.ui.visibility` excludes `model`
are omitted from `tools`; trusted hosts can invoke them through `tool(name)`.

For an installed external launch wrapper, set `NANOCODEX_COMPUTER` to its absolute
path and `NANOCODEX_COMPUTER_TRANSPORT=mcp`. Native discovery and JavaScript desktop
attachments use that wrapper without companion flags. Programmatic native callers
can instead use `ComputerConfig::mcp(executable)`, set `args` and `environment`, and
await `ComputerTools::connect`; JavaScript callers pass the same trusted options
to `connectComputerTools`. External provider timeouts remain provider-owned.

Managed `select_computer` returns the selected provider's exact declarations.
Read those before calling CUA. Screen-only Mac, Windows, Linux and phone hosts
are unsupported by this CUA path; their hardware publishers remain separate.
The managed namespace does not publish `computer`.

Hosts with a genuine user-facing form UI can provide `elicitationHandler`:

```js
const computer = await connectComputerTools({
  executable: providerExecutable,
  args: providerArgs,
  transport: "mcp",
  elicitationTimeoutMs: 300_000,
  elicitationHandler: async (params, context) => {
    // Host-owned UI: display params.message/requestedSchema and the provider's
    // _meta approval details; dismiss when context.signal is aborted.
    return await showHostForm(params, context);
    // Return { action: "accept" | "decline" | "cancel", content?, _meta? }.
  },
});
```

Only a configured handler advertises MCP `elicitation.form`. The adapter accepts
`elicitation/create` and `openai/elicitation/create` form requests (omitted `mode` also means form), forwards all
raw parameters including `_meta`, and preserves the host's response metadata.
It never selects acceptance or persistence. Hosts must obtain the user's choice
before returning `accept` or `_meta.persist`; provider metadata is presentation
input, not authorization. URL requests and other extension methods are unsupported.
Without a handler, server requests receive a method-not-found error.

Context contains the provider `requestId`, `signal`, and the active tool's
`sessionId`, `callId`, and `model`. Discovery-time requests have no active tool
identity. `elicitationTimeoutMs` is a positive safe integer, defaults to 300 seconds,
and bounds the form wait independently of provider-owned tool execution timeouts.
Expiry and provider cancellation abort the signal and return `cancel`. Caller
abort, requesting call completion, session release, process exit, and attachment close also abort pending
forms. Hosts must use the signal to dismiss their UI; late responses are ignored.
Thrown handlers return an internal error without exposing the host exception.

The current desktop app does not yet supply this callback: its JSONL bridge has
no form-response action or native form presentation route. It therefore does not
advertise this capability. Adding an adapter callback alone does not enable
interactive approval in an installed desktop app.
