# React + Vite Worker example

This app embeds the browser build of `nanocodex-wasm` in a module Worker. The
Worker owns one persistent `Nanocodex` session, forwards its ordered events to
React, and registers the browser-native `browserInfo` tool.

```sh
just bootstrap-bindings
just build-react-example
just dev-react-example
```

`just dev-react-example` starts the Vite app with the existing OpenAI API-key
path selected by default. That path uses the Cloudflare Worker upgrade proxy;
the key never enters the page or browser Worker.

`pnpm --filter nanocodex-react-vite-example build` also checks the generated
chunk graph. The default OpenAI entry
must remain below 220 KiB, must not preload the Tempo wallet integration, and
the explicit MPP UI and Worker paths must remain lazy entries.

Selecting **Tempo MPP** opts into the keyless payment path. Only then does the
page dynamically load the Tempo Accounts SDK and show Tempo-specific UI. The
page authorizes a scoped access key, then the dedicated Worker dynamically loads
the MPP integration from same-origin IndexedDB and opens
`wss://openai.mpp.tempo.xyz/v1/responses`. The normal OpenAI bundle and runtime
do not initialize a wallet, access key, or payment session.

Marking the model manager as a Nanocodex Tempo provider automatically enables
the built-in Mercator MCP at `https://mercator.tempo.xyz/mcp`, backed by the
same delegated signer and persistent channel store. Nanocodex exposes
only provider-native `tool_search` to the model initially. Mercator's tools stay
deferred under `mcp__mercator__*`, become discoverable through BM25 search, and
are invoked from Code Mode. MPPx wraps the MCP SDK client and automatically
answers Tempo payment challenges, including paid flows Mercator composes from
its service catalog. The UI reports model and Mercator cumulative payments
separately.

Try this after starting the Tempo MPP agent:

```text
Use tool_search to discover the Mercator tools. Then, only from Code Mode,
search for a low-cost service that returns the current Tempo chain block number
and describe the best match. Build and quote a one-node flow with totalMax and
maxSpend both set to 0.01. If the aggregate quote is at most 0.01 pathUSD,
submit it with a unique idempotency key and poll the job until it completes.
Report the chosen service, quote, result, model MPP payment, and
Mercator/downstream MPP payment. Never call mcp__mercator__* as a top-level
model tool.
```

In either mode the Worker owns one persistent Nanocodex agent. In MPP mode it
also owns one persistent MPP manager, which reuses its paid channel. Serialized
channel state is retained in IndexedDB and can be recovered after a Worker or
page restart. Tempo wallet, signer, channel, and payment diagnostics are rendered
only while MPP is selected; raw ordered agent events are available in both modes.
