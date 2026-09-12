# nanocodex-computer (experimental)

Node attachment adapter for the persistent Nanocodex CUA companion. It exports
the same `mcp__cua_repl__js` / `mcp__cua_repl__js_reset` function contract as
Codex's `cua_repl` MCP server and the Rust integration.

```js
import { createComputerTools, discoverComputer } from "nanocodex-computer";

const executable = await discoverComputer();
if (!executable) throw new Error("Install the companion with pnpm install:computer");
const computer = createComputerTools({ executable });
// Add computer.tools to your existing createTools({ tools, workspace }) call.
// Close the attachment before completing its resource cleanup:
await computer.close();
```

Each conversation has its own persistent process. Calls are serialized, and
cancellation stops the process before another call can continue. A failed
transport requires `cua_repl.js_reset`. The named tools implement `releaseSession` and
`dispose` for attachment cleanup. Screenshot results are API image content with
original detail, including when consumed through Code Mode.

`createComputerTools` accepts trusted `args`, `environment` and a Linux Hand's
private `desktopRuntime` directory. Those are host configuration, not model
arguments. The child environment omits account/API credentials.

`nanocodex-computer/contract` exports the tool descriptions, JSON schemas and
input validator without importing Node APIs, for hosted Workers and brokers.

See the [runtime and integration documentation](../../crates/experimental/nanocodex-computer/README.md)
for OS requirements, CDP setup, release packaging and validation commands.
