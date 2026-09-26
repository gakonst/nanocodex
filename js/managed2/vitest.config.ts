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
      bindings: { RESPONSES_TRANSPORT: process.env.MANAGED2_TEST_TRANSPORT === "websocket" ? "websocket" : "http",
        ...(process.env.MANAGED2_TEST_ASYNC_TOOLS === "true" ? { NANOCODEX_TEST_ASYNC_TOOLS: "true" } : {}),
        AUTH_API_KEY_HASHES: JSON.stringify(Object.fromEntries(
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
            if (url.href === "https://api.openai.com/v1/alpha/search" && request.method === "POST") {
              if (request.headers.get("authorization") !== "Bearer sk-fixture-only"
                || request.headers.has("x-managed2-owner") || request.headers.has("x-managed2-trace-id")) {
                return Response.json({ error: "search authentication or privacy failure" }, { status: 401 });
              }
              const body = await request.json();
              if (body.settings?.external_web_access !== true || body.settings?.allowed_callers?.[0] !== "direct"
                || !(["a synthetic question", "a synthetic async question", "a synthetic active question", "a synthetic cohort question", "a synthetic mixed fast question", "a synthetic mixed slow question"].includes(body.commands?.search_query?.[0]?.q))) {
                return Response.json({ error: "invalid search body" }, { status: 400 });
              }
              if (["a synthetic async question", "a synthetic mixed slow question"].includes(body.commands?.search_query?.[0]?.q))
                await new Promise(resolve => setTimeout(resolve, 2500));
              if (body.commands?.search_query?.[0]?.q === "a synthetic active question")
                await new Promise(resolve => setTimeout(resolve, 120));
              if (body.commands?.search_query?.[0]?.q === "a synthetic mixed fast question")
                await new Promise(resolve => setTimeout(resolve, 400));
              return Response.json({ output: "Found [synthetic citation](https://example.org/source)"
                + (body.commands?.search_query?.[0]?.q === "a synthetic async question" ? " [async fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic active question" ? " [active fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic cohort question" ? " [cohort fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic mixed fast question" ? " [mixed-fast fixture]" : ""), hidden: "provider-only" });
            }
            if (url.href === "https://chatgpt.com/backend-api/codex/alpha/search" && request.method === "POST") {
              if (request.headers.get("chatgpt-account-id") !== "account-fixture"
                || request.headers.has("x-managed2-owner") || !request.headers.get("authorization")?.startsWith("Bearer eyJ")) {
                return Response.json({ error: "subscription search authentication failure" }, { status: 401 });
              }
              const body = await request.json();
              if (!(["a synthetic question", "a synthetic async question", "a synthetic active question", "a synthetic cohort question", "a synthetic mixed fast question", "a synthetic mixed slow question"].includes(body.commands?.search_query?.[0]?.q))) {
                return Response.json({ error: "invalid search body" }, { status: 400 });
              }
              if (["a synthetic async question", "a synthetic mixed slow question"].includes(body.commands?.search_query?.[0]?.q))
                await new Promise(resolve => setTimeout(resolve, 2500));
              if (body.commands?.search_query?.[0]?.q === "a synthetic active question")
                await new Promise(resolve => setTimeout(resolve, 120));
              if (body.commands?.search_query?.[0]?.q === "a synthetic mixed fast question")
                await new Promise(resolve => setTimeout(resolve, 400));
              return Response.json({ output: "Found [synthetic citation](https://example.org/source)"
                + (body.commands?.search_query?.[0]?.q === "a synthetic async question" ? " [async fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic active question" ? " [active fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic cohort question" ? " [cohort fixture]" : "")
                + (body.commands?.search_query?.[0]?.q === "a synthetic mixed fast question" ? " [mixed-fast fixture]" : ""), hidden: "provider-only" });
            }
            const platform = url.hostname === "api.openai.com"
              && url.pathname === "/v1/responses"
              && request.headers.get("authorization") === "Bearer sk-fixture-only"
              && !request.headers.has("x-nanocodex-egress-request-id")
              && !request.headers.has("x-managed2-trace-id");
            const subscription = url.hostname === "chatgpt.com"
              && url.pathname === "/backend-api/codex/responses"
              && request.headers.get("authorization") === "Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZpeHR1cmUiLCJjaGF0Z3B0X2FjY291bnRfaXNfZmVkcmFtcCI6ZmFsc2V9fQ.fixture"
              && request.headers.get("chatgpt-account-id") === "account-fixture"
              && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(request.headers.get("x-nanocodex-egress-request-id") || "")
              && !request.headers.has("x-managed2-trace-id");
            if (!platform && !subscription) {
              return new Response("bad upstream authentication", { status: 401 });
            }
            const respond = async body => {
              const input = body.input || [];
              const latestUser = input.findLastIndex(item => item.role === "user");
              const currentTurn = input.slice(Math.max(0, latestUser));
              const shellContinuation = currentTurn.find(item => item.type === "function_call_output" && item.call_id === "call-shell");
              const shellTool = input.find(item => item.type === "additional_tools")?.tools?.find(tool => tool.name === "exec_command");
              const shellMatch = JSON.stringify(currentTurn).match(/Use exec_command: ([^"\\\\]+)/);
              if (shellContinuation) {
                let result;
                try { result = JSON.parse(shellContinuation.output); } catch { result = shellContinuation.output; }
                const text = "Shell: " + JSON.stringify(result);
                return [{ type: "response.completed", response: { id: "fixture-shell-result", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (shellMatch) {
                if (!shellTool) throw new Error("exec_command was not offered to the provider");
                return [{ type: "response.completed", response: { id: "fixture-shell-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-shell", name: "exec_command", arguments: JSON.stringify({ cmd: shellMatch[1] }) }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              // A fast completed job and a cancelled in-flight job must be
              // delivered together; late upstream completion cannot overwrite
              // the uncertain cancellation result.
              const mixedA = input.filter(item => item.type === "function_call_output" && item.call_id === "call-mixed-a");
              const mixedB = input.filter(item => item.type === "function_call_output" && item.call_id === "call-mixed-b");
              if (mixedA.length || mixedB.length) {
                const complete = mixedA.some(item => String(item.output).includes("[mixed-fast fixture]"));
                const uncertain = mixedB.some(item => String(item.output).includes("Cancellation requested after dispatch"));
                const pendingA = mixedA.some(item => String(item.output).includes("Tool call is still running."));
                const pendingB = mixedB.some(item => String(item.output).includes("Tool call is still running."));
                if (complete !== uncertain) throw new Error("mixed terminal cohort was split across requests");
                if (!complete && (!pendingA || !pendingB)) throw new Error("mixed pending cohort was incomplete");
                const text = complete ? "Mixed cohort complete and uncertain together"
                  : "Waiting for mixed cohort";
                return [{ type: "response.completed", response: { id: "fixture-mixed-result", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (JSON.stringify(input).includes("Use async mixed cohort")) {
                return [{ type: "response.completed", response: { id: "fixture-mixed-calls", status: "completed", end_turn: false,
                  output: ["fast", "slow"].map(kind => ({ type: "function_call", call_id: "call-mixed-" + (kind === "fast" ? "a" : "b"),
                    name: "web__run", arguments: JSON.stringify({ search_query: [{ q: "a synthetic mixed " + kind + " question" }] }) })),
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              // Two independently completed, read-only jobs from one source
              // turn must be coalesced into a single prompt-less idle wake.
              const cohortA = input.filter(item => item.type === "function_call_output" && item.call_id === "call-cohort-a");
              const cohortB = input.filter(item => item.type === "function_call_output" && item.call_id === "call-cohort-b");
              const cohortTerminal = item => String(item.output).includes("[cohort fixture]");
              if (cohortA.length || cohortB.length) {
                const terminalA = cohortA.some(cohortTerminal);
                const terminalB = cohortB.some(cohortTerminal);
                const pendingA = cohortA.some(item => String(item.output).includes("Tool call is still running."));
                const pendingB = cohortB.some(item => String(item.output).includes("Tool call is still running."));
                if (terminalA !== terminalB) throw new Error("idle cohort terminal outputs split across model requests");
                if (!terminalA && (!pendingA || !pendingB)) throw new Error("idle cohort pending output missing");
                const text = terminalA ? "Cohort terminal outputs together"
                  : "Waiting for coalesced background searches";
                return [{ type: "response.completed", response: { id: "fixture-cohort-result", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (JSON.stringify(input).includes("Use async cohort")) {
                return [{ type: "response.completed", response: { id: "fixture-cohort-calls", status: "completed", end_turn: false,
                  output: ["call-cohort-a", "call-cohort-b"].map(call_id => ({ type: "function_call", call_id,
                    name: "web__run", arguments: JSON.stringify({ search_query: [{ q: "a synthetic cohort question" }] }) })),
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              // Active-boundary scenario: a fast read-only tool settles while
              // a slow provider response is still in flight. A second tool
              // forces a next model request in the *original* turn. It must
              // include the terminal output under call-active-web, not an
              // idle synthetic prompt after the source turn already ended.
              const activeWeb = input.filter(item => item.type === "function_call_output" && item.call_id === "call-active-web");
              const activeTime = input.filter(item => item.type === "function_call_output" && item.call_id === "call-active-time");
              const activeTerminal = activeWeb.findLast(item => String(item.output).includes("[active fixture]"));
              const activePending = activeWeb.find(item => String(item.output).includes("Tool call is still running."));
              if (activeTerminal) {
                if (!activeTime.length) throw new Error("active boundary lacked second tool output");
                const text = "Active boundary terminal: " + activeTerminal.output;
                return [{ type: "response.completed", response: { id: "fixture-active-final", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (activePending) {
                await new Promise(resolve => setTimeout(resolve, 2200));
                return [{ type: "response.completed", response: { id: "fixture-active-time-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-active-time", name: "current_time", arguments: "{}" }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (activeTime.length) {
                return [{ type: "response.completed", response: { id: "fixture-active-missed", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Active boundary missed terminal" }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (JSON.stringify(input).includes("Use async active boundary")) {
                return [{ type: "response.completed", response: { id: "fixture-active-web-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-active-web", name: "web__run",
                    arguments: JSON.stringify({ search_query: [{ q: "a synthetic active question" }] }) }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              const continuation = input.find(item => item.type === "function_call_output" && item.call_id === "call-time");
              const timeTool = input.find(item => item.type === "additional_tools")?.tools?.find(tool => tool.name === "current_time");
              const webTool = input.find(item => item.type === "additional_tools")?.tools?.find(tool => tool.name === "web__run");
              const webOutputs = input.filter(item => item.type === "function_call_output" && item.call_id === "call-web");
              const webTerminal = webOutputs.findLast(item => String(item.output).includes("synthetic citation")
                || String(item.output).includes("Cancellation requested after dispatch; side effect may have occurred"));
              const webPending = webOutputs.find(item => String(item.output).includes("Tool call is still running."));
              // WebSocket continuation sends only the new output and may omit
              // the original user text; an original-call-ID pending marker is
              // the deterministic async fixture discriminator in that path.
              const asyncWeb = JSON.stringify(input).includes("Use async web__run")
                || Boolean(webPending) || Boolean(webTerminal && (String(webTerminal.output).includes("[async fixture]")
                  || String(webTerminal.output).includes("Cancellation requested after dispatch")));
              // A pending result must be attached to the original provider
              // call ID; terminal delivery must use that ID again rather than
              // a forged user turn or a different synthetic call. This fixture
              // deliberately distinguishes pending and terminal requests.
              if (webTerminal || webPending) {
                const uncertain = Boolean(webTerminal && String(webTerminal.output).includes("Cancellation requested after dispatch"));
                const text = asyncWeb
                  ? uncertain ? "Background job uncertain: " + webTerminal.output
                    : webTerminal ? "Background search finished: " + webTerminal.output : "Waiting for background search"
                  : "Search: " + (webTerminal ?? webOutputs[0]).output;
                return [{ type: "response.completed", response: { id: uncertain ? "fixture-web-uncertain" : webTerminal ? "fixture-web-terminal" : "fixture-web-pending", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (asyncWeb) {
                // A second provider request without the original call's
                // pending or terminal output is a protocol violation, not a
                // license to invent a second call or pass a forged user turn.
                if (input.some(item => item.type === "function_call" && item.call_id === "call-web"))
                  throw new Error("async provider continuation lacked original-call-ID output");
                if (!webTool) throw new Error("web__run was not offered");
                return [{ type: "response.completed", response: { id: "fixture-async-web-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-web", name: "web__run",
                    arguments: JSON.stringify({ search_query: [{ q: "a synthetic async question" }] }) }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              if (JSON.stringify(input).includes("Use web__run")) {
                if (!webTool) throw new Error("web__run was not offered");
                return [{ type: "response.completed", response: { id: "fixture-web-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-web", name: "web__run",
                    arguments: JSON.stringify({ search_query: [{ q: "a synthetic question" }] }) }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              const requested = JSON.stringify(input).includes("Use current_time");
              if (continuation) {
                const utc = JSON.parse(continuation.output).utc;
                const text = "Current UTC: " + utc;
                return [
                  { type: "response.output_item.added", output_index: 0, item: { id: "fixture-message", type: "message", role: "assistant", status: "in_progress", content: [{ type: "output_text", text: "" }] } },
                  { type: "response.output_text.delta", output_index: 0, item_id: "fixture-message", content_index: 0, delta: text },
                  { type: "response.completed", response: { id: "fixture-time-result", status: "completed", end_turn: true,
                    output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                    usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } },
                ];
              }
              if (requested) {
                if (!timeTool) throw new Error("current_time was not offered to the provider");
                return [{ type: "response.completed", response: { id: "fixture-time-call", status: "completed", end_turn: false,
                  output: [{ type: "function_call", call_id: "call-time", name: "current_time", arguments: "{}" }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } }];
              }
              const text = "hello from test model";
              return [
                { type: "response.output_item.added", output_index: 0, item: { id: "fixture-message", type: "message", role: "assistant", status: "in_progress", content: [{ type: "output_text", text: "" }] } },
                { type: "response.output_text.delta", output_index: 0, item_id: "fixture-message", content_index: 0, delta: text },
                { type: "response.completed", response: { id: "fixture-response", status: "completed", end_turn: true,
                  output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
                  usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 } } },
              ];
            };
            if (request.method === "POST") {
              const frames = await respond(await request.json());
              return new Response(frames.map(frame => "data: " + JSON.stringify(frame) + "\\n\\n").join(""),
                { status: 200, headers: { "content-type": "text/event-stream" } });
            }
            const pair = new WebSocketPair();
            const [client, server] = Object.values(pair);
            server.accept();
            server.addEventListener("message", event => {
              const frame = JSON.parse(event.data);
              void respond(frame.response ?? frame).then(frames => {
                for (const reply of frames) server.send(JSON.stringify(reply));
              }, () => server.send(JSON.stringify({ type: "error", error: { message: "synthetic provider fixture rejected request" } })));
            });
            return new Response(null, { status: 101, webSocket: client });
          } }
        ` },
      ],
    },
  })],
  test: { include: ["test/**/*.test.ts"] },
});
