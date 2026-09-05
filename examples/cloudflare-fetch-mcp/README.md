# Nanocodex fetch API on Cloudflare Workers

This is the small hosted counterpart to the browser demo. A client sends one
ordinary authenticated `fetch` request. A Cloudflare Durable Object runs the
real Rust/WASM Nanocodex loop server-side, pays for the model over Tempo MPP,
uses Tempo provider mode to enable the package's built-in Mercator MCP,
discovers its tools through provider-native `tool_search`, and executes
deferred `mcp__mercator__*` calls inside QuickJS-backed Code Mode.

QuickJS is required because Cloudflare Workers reject request-time `eval` and
`new Function`. Only Nanocodex's explicit Code Mode globals cross into the
interpreter, and the evaluator enforces memory, stack, and execution-cycle
limits. The Durable Object serializes prompts for the hosted wallet and stores
MPP channels in durable storage, preventing concurrent voucher races while
still allowing every caller to use a plain HTTP request.

## Local run

Build the browser WASM binding once, install this example, and create
`examples/cloudflare-fetch-mcp/.dev.vars` with a funded Tempo private key:

```sh
just build-wasm
npm install --prefix examples/cloudflare-fetch-mcp

cat > examples/cloudflare-fetch-mcp/.dev.vars <<'VARS'
API_TOKEN=local-demo-token
TEMPO_PRIVATE_KEY=0xYOUR_32_BYTE_PRIVATE_KEY
VARS

npm run dev --prefix examples/cloudflare-fetch-mcp
node examples/cloudflare-fetch-mcp/client.mjs
```

Equivalent curl:

```sh
curl --fail-with-body http://127.0.0.1:8787/v1/fetch \
  -H 'Authorization: Bearer local-demo-token' \
  -H 'Content-Type: application/json' \
  --data '{
    "prompt": "Use tool_search to find Mercator service discovery. Find three Tempo data services, summarize them, and identify any paid follow-up.",
    "thinking": "high"
  }'
```

The JSON response contains `final_message`, model usage, and separate cumulative
amounts for the model and Mercator MPP channels. Paid MCP credentials and wallet
secrets never enter the prompt, model-visible tool definitions, or client.

## Deploy

```sh
cd examples/cloudflare-fetch-mcp
npx wrangler secret put API_TOKEN
npx wrangler secret put TEMPO_PRIVATE_KEY
npm run check
npx wrangler deploy
```

The server wallet is a spending credential. Fund it narrowly, use the configured
per-channel deposit caps, protect the fetch endpoint, and rotate both secrets as
you would any production payment service.
