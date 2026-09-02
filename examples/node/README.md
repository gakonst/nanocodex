# Node.js PoC

This example consumes the publishable `nanocodex` package exactly like an
external Node application. The Node host supplies the WebSocket, API key, and
an ordinary JavaScript `multiply` tool; the Rust/WASM engine owns the agent
lifecycle, tool loop, retained conversation, and follow-on response chain.
Each accepted Turn resolves to a typed result containing `finalMessage`,
`usage`, and a reusable session `snapshot`; this example prints only the final
message.

From the repository root:

```sh
pnpm install --frozen-lockfile
OPENAI_API_KEY=... pnpm --filter nanocodex-node-example start
```

The start command also reads the repository's ignored `.env` file when present. The
key remains in the Node process and is used by the Node WebSocket host; it is
not compiled into the WASM artifact or the npm package.

The subagent example opts into the canonical Rust task tree compiled into the
package's WASM. JavaScript spreads `Subagents.create()` into `tools`; Rust installs
`spawn_agent`, `submit_result`, `send_agent_message`, `list_agents`,
`wait_agent`, `interrupt_agent`, and `close_agent` for every child:

```sh
pnpm --filter nanocodex-node-example subagents -- "Review the JS API with whatever workers you need"
```

To run the keyless MPP path with a Tempo account managed by the Tempo Accounts
SDK:

```sh
pnpm --filter nanocodex-node-example smoke:mpp
pnpm --filter nanocodex-node-example smoke:mpp -- "Explain MPP in one sentence."
```

The first run prints a Tempo Wallet device-code URL to authorize a locally
persisted, one-day access key with 25 pathUSD and USDC.e limits. Later runs
reuse that scoped P-256 key without asking the root wallet to sign each
payment. The root wallet and delegated signer addresses are printed to stderr.

Stdout is flushed JSONL from `agent.events.watch()`; model output, payment
diagnostics, and settlement details go to stderr so redirecting stdout produces
a directly parseable trace:

```sh
pnpm --filter nanocodex-node-example smoke:mpp -- "Explain MPP in one sentence." > events.jsonl
jq -c . events.jsonl >/dev/null
```

The smoke uses standard reasoning with no thinking and priority processing,
auto-swaps into the service currency, caps its payment channel at 0.05, opens a
paid Responses WebSocket, and verifies a model turn. Channel state is persisted
under `~/.tempo/wallet/nanocodex-mpp-channels.json` and reused by default. Pass
`--close` when you explicitly want to cooperatively settle instead:

```sh
pnpm --filter nanocodex-node-example smoke:mpp -- --close "Finish this turn and close the payment channel."
```
