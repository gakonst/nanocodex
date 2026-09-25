import { createHash } from "node:crypto";
import { fixtureKeys } from "./test/fixtures/auth.ts";
import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";
import { build } from "esbuild";

// The secondary Worker keeps real routing/DO code. Its Rust subscription
// runtime is tested in egress2 itself; this multi-Worker test substitutes a
// deterministic subscription manager to avoid loading two WASM modules here.
const egressScript = (await build({
  entryPoints: [new URL("../egress2/src/index.ts", import.meta.url).pathname],
  bundle: true, format: "esm", platform: "browser", write: false,
  external: ["cloudflare:workers"],
  plugins: [{ name: "subscription-fixture", setup(build) {
    build.onResolve({ filter: /^\.\/subscriptionRuntime$/ }, args => args.importer.endsWith("/egress2/src/index.ts")
      ? { path: new URL("test/fixtures/subscription-runtime.ts", import.meta.url).pathname }
      : undefined);
    build.onResolve({ filter: /^\.\/relay$/ }, args => args.importer.endsWith("/egress2/src/index.ts")
      ? { path: new URL("test/fixtures/relay.ts", import.meta.url).pathname }
      : undefined);
  } }],
})).outputFiles[0]!.text;

export default defineConfig({
  plugins: [cloudflareTest({
    wrangler: { configPath: "./wrangler.jsonc" },
    miniflare: {
      bindings: { RESPONSES_TRANSPORT: process.env.MANAGED2_TEST_TRANSPORT === "websocket" ? "websocket" : "http", AUTH_API_KEY_HASHES: JSON.stringify(Object.fromEntries(
        Object.entries(fixtureKeys).map(([owner, key]) => [createHash("sha256").update(key).digest("base64url"), owner]),
      )) },
      serviceBindings: { EGRESS: { name: "nanocodex-egress2" } },
      workers: [
        { name: "nanocodex-egress2", modules: true, script: egressScript, compatibilityDate: "2026-07-29", outboundService: "test-provider",
          durableObjects: { USER_CREDENTIALS: { className: "UserCredentials", useSQLite: true } },
          bindings: { CREDENTIAL_ENCRYPTION_KEY: btoa("0123456789abcdef0123456789abcdef") } },
        { name: "test-provider", modules: true, script: `
          export default { async fetch(request) {
            const url = new URL(request.url);
            const platform = url.hostname === "api.openai.com"
              && url.pathname === "/v1/responses"
              && request.headers.get("authorization") === "Bearer sk-fixture-only";
            const subscription = url.hostname === "chatgpt.com"
              && url.pathname === "/backend-api/codex/responses"
              && request.headers.get("authorization") === "Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZpeHR1cmUiLCJjaGF0Z3B0X2FjY291bnRfaXNfZmVkcmFtcCI6ZmFsc2V9fQ.fixture"
              && request.headers.get("chatgpt-account-id") === "account-fixture";
            if (!platform && !subscription) {
              return new Response("bad upstream authentication", { status: 401 });
            }
            if (request.method === "POST") return new Response("data: " + JSON.stringify({ type: "response.completed", response: {
              id: "fixture-response", status: "completed", end_turn: true,
              output: [{ type: "message", role: "assistant", content: [
                { type: "output_text", text: "hello from test model" }
              ] }], usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 }
            } }) + "\\n\\n", { status: 200, headers: { "content-type": "text/event-stream" } });
            const pair = new WebSocketPair();
            const [client, server] = Object.values(pair);
            server.accept();
            server.addEventListener("message", () => {
              server.send(JSON.stringify({ type: "response.completed", response: {
                id: "fixture-response", status: "completed", end_turn: true,
                output: [{ type: "message", role: "assistant", content: [
                  { type: "output_text", text: "hello from test model" }
                ] }], usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 }
              } }));
            });
            return new Response(null, { status: 101, webSocket: client });
          } }
        ` },
      ],
    },
  })],
  test: { include: ["test/**/*.test.ts"] },
});
