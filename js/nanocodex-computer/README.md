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
companion-specific flags are not added in that mode. This attachment supports
the bundled argument schemas and rejects other schemas explicitly; use generic
MCP registration for a provider with different tools or arguments.

Managed `select_computer` returns the selected provider's exact declarations.
Read those before calling CUA. Screen-only Mac, Windows, Linux and phone hosts
are unsupported by this CUA path; their hardware publishers remain separate.
The managed namespace does not publish `computer`.
