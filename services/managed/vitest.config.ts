import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

const TEST_BROKER = `
const subjects = new Set();
const connectors = new Map();
let heldSubject;
let holdSubjectBind = false;
let heldSubjectBinds = 0;
let heldSubjectUnbinds = 0;
let heldSubjectResponses = 0;
let heldSubjectOrder = [];
export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/test/hold-subject-bind") {
      if (request.method === "POST") {
        let body;
        try { body = await request.json(); } catch {}
        heldSubject = typeof body?.subject === "string" ? body.subject : undefined;
        heldSubjectBinds = 0;
        heldSubjectUnbinds = 0;
        heldSubjectResponses = 0;
        heldSubjectOrder = [];
        holdSubjectBind = body?.hold !== false;
        return new Response(null, { status: 204 });
      }
      if (request.method === "GET") {
        return Response.json({
          binds: heldSubjectBinds,
          order: heldSubjectOrder,
          responses: heldSubjectResponses,
          subject: heldSubject,
          unbinds: heldSubjectUnbinds,
        });
      }
      if (request.method === "DELETE") {
        holdSubjectBind = false;
        return new Response(null, { status: 204 });
      }
      return Response.json({ error: "method_not_allowed" }, { status: 405 });
    }
    const connectorRoute = url.pathname.match(/^\\/users\\/([^/]+)\\/connectors(?:\\/(github|gmail|gdrive|x|whoop)(\\/callback)?)?$/);
    if (connectorRoute) {
      const [, userId, connector, callback] = connectorRoute;
      if (!connector && request.method === "GET") {
        const connected = connectors.get(userId) || new Map();
        return Response.json({ connectors: Object.fromEntries(
          ["github", "gmail", "gdrive", "x", "whoop"].map((id) => [id, connected.has(id)
            ? { connected: true, account_id: id + "-account", label: connected.get(id) }
            : { connected: false }]),
        ) });
      }
      if (connector && !callback && request.method === "POST") {
        const body = await request.json();
        const state = userId + "-" + connector + "-state";
        connectors.set("pending:" + state, { userId, connector, returnTo: body.return_to });
        return Response.json({
          authorization_url: "https://provider.test/authorize?" + new URLSearchParams({
            redirect_uri: body.redirect_uri,
            state,
          }),
        });
      }
      if (connector && callback && request.method === "POST") {
        const body = await request.json();
        const pending = connectors.get("pending:" + body.state);
        if (!pending || pending.userId !== userId || pending.connector !== connector) {
          return Response.json({ error: "invalid_oauth_state" }, { status: 400 });
        }
        connectors.delete("pending:" + body.state);
        if (body.error) return Response.json({ connected: false, return_to: pending.returnTo });
        const connected = connectors.get(userId) || new Map();
        connected.set(connector, connector === "github" ? "Nano Cat" : connector + "@example.test");
        connectors.set(userId, connected);
        return Response.json({ connected: true, return_to: pending.returnTo });
      }
      if (connector && !callback && request.method === "DELETE") {
        connectors.get(userId)?.delete(connector);
        return new Response(null, { status: 204 });
      }
      return Response.json({ error: "method_not_allowed" }, { status: 405 });
    }
    const subjectRoute = url.pathname.match(/^\\/subjects\\/([A-Za-z0-9_-]{43,128})$/);
    if (subjectRoute && request.method === "PUT") {
      const body = await request.json();
      if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(body?.user_id ?? "")) {
        return Response.json({ error: "invalid_request" }, { status: 400 });
      }
      if (holdSubjectBind && heldSubject === undefined) heldSubject = subjectRoute[1];
      if (subjectRoute[1] === heldSubject) {
        heldSubjectBinds += 1;
        heldSubjectOrder.push("bind");
      }
      if (holdSubjectBind && subjectRoute[1] === heldSubject) {
        while (holdSubjectBind) await scheduler.wait(1);
      }
      subjects.add(subjectRoute[1]);
      return new Response(null, { status: 204 });
    }
    if (subjectRoute && request.method === "DELETE") {
      if (subjectRoute[1] === heldSubject) {
        heldSubjectUnbinds += 1;
        heldSubjectOrder.push("unbind");
      }
      subjects.delete(subjectRoute[1]);
      return new Response(null, { status: 204 });
    }
    const authorization = request.headers.get("authorization");
    const subject = request.headers.get("x-nanocodex-subject");
    const modelStatus = url.href === "https://broker.internal/.well-known/nanocodex/model-status"
      && request.method === "GET"
      && typeof subject === "string"
      && subjects.has(subject);
    if (modelStatus) return Response.json({ ready: true });
    const browserModel = url.hostname === "nanocodex.internal"
      && [
        "/v1/responses",
        "/v1/search",
        "/v1/images/generations",
        "/v1/images/edits",
        "/v1/realtime/calls",
        "/v1/realtime/sideband",
      ].includes(url.pathname)
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string";
    if (browserModel && !subjects.has(subject)) {
      return Response.json({ error: "agent_subject_unavailable" }, { status: 403 });
    }
    if (url.href === "https://api.github.com/repos/gakonst/nanocodex"
      && request.method === "GET"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject)) {
      return Response.json({
        cookie: request.headers.get("cookie"),
        full_name: "gakonst/nanocodex",
        subject,
      });
    }
    const search = url.href === "https://nanocodex.internal/v1/search"
      && request.method === "POST"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject)
      && request.headers.get("chatgpt-account-id") === null;
    if (search) {
      const body = await request.text();
      let value;
      try { value = JSON.parse(body); } catch {}
      if (typeof value?.id === "string"
        && value?.model === "gpt-5.6-sol"
        && value?.commands?.search_query?.[0]?.q === "managed web"
        && value?.settings?.allowed_callers?.[0] === "direct"
        && value?.settings?.external_web_access === true
        && value?.max_output_tokens === 10000) {
        return Response.json({ output: "MANAGED_WEB_SEARCH_OK" });
      }
      return Response.json({
        body,
        cookie: request.headers.get("cookie"),
        origin: request.headers.get("origin"),
        subject,
      });
    }
    const realtimeCall = url.href === "https://nanocodex.internal/v1/realtime/calls"
      && request.method === "POST"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject)
      && request.headers.get("chatgpt-account-id") === null;
    if (realtimeCall) {
      const agent = request.headers.get("x-nanocodex-agent-id");
      return Response.json({
        agent,
        body: await request.text(),
        cookie: request.headers.get("cookie"),
        lifecycleSession: request.headers.get("session-id"),
        openAiAlpha: request.headers.get("openai-alpha"),
        origin: request.headers.get("origin"),
        session: request.headers.get("x-session-id"),
        subject,
        thread: request.headers.get("thread-id"),
      }, { headers: {
        location: "/backend-api/codex/realtime/calls/rtc_test",
        ...(agent === null ? {} : {
          authorization: "Bearer provider-secret",
          "chatgpt-account-id": "provider-account",
          "set-cookie": "provider=session",
        }),
      } });
    }
    const realtimeSideband = url.href === "https://nanocodex.internal/v1/realtime/sideband"
      && request.method === "GET"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject)
      && request.headers.get("chatgpt-account-id") === null
      && request.headers.get("upgrade")?.toLowerCase() === "websocket";
    if (realtimeSideband) {
      const pair = new WebSocketPair();
      const [client, server] = Object.values(pair);
      server.accept();
      queueMicrotask(() => server.send(JSON.stringify({
        agent: request.headers.get("x-nanocodex-agent-id"),
        callId: request.headers.get("x-nanocodex-realtime-call-id"),
        cookie: request.headers.get("cookie"),
        lifecycleSession: request.headers.get("session-id"),
        openAiAlpha: request.headers.get("openai-alpha"),
        session: request.headers.get("x-session-id"),
        subject,
        thread: request.headers.get("thread-id"),
      })));
      return new Response(null, { status: 101, webSocket: client });
    }
    const image = url.href === "https://nanocodex.internal/v1/images/generations"
      && request.method === "POST"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject);
    if (image) {
      const value = await request.json();
      if (value?.model === "gpt-image-2" && value?.prompt === "draw managed") {
        return Response.json({ data: [{ b64_json: "TUFOQUdFRF9JTUFHRV9PSw==" }] });
      }
      return Response.json({ error: { message: "invalid managed image request" } }, { status: 400 });
    }
    const responses = url.href === "https://nanocodex.internal/v1/responses"
      && authorization === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
      && typeof subject === "string"
      && subjects.has(subject)
      && request.headers.get("chatgpt-account-id") === null;
    if (request.method !== "GET"
      || request.headers.get("upgrade")?.toLowerCase() !== "websocket"
      || request.headers.get("openai-beta") !== "responses_websockets=2026-02-06"
      || !responses) {
      return Response.json({ error: "test_broker_denied" }, { status: 403 });
    }
    const pair = new WebSocketPair();
    if (subject === heldSubject) {
      heldSubjectResponses += 1;
      heldSubjectOrder.push("transport");
    }
    const [client, server] = Object.values(pair);
    server.accept();
    let pendingResponse;
    server.addEventListener("message", (event) => {
      let command;
      try { command = JSON.parse(String(event.data)); } catch { return; }
      if (command.type === "response.cancel") {
        if (pendingResponse !== undefined) clearTimeout(pendingResponse);
        pendingResponse = undefined;
        return;
      }
      const input = Array.isArray(command.input) ? command.input : [];
      const messages = input.filter((item) => item?.type === "message" && item.role === "user");
      const latest = messages.at(-1);
      const content = Array.isArray(latest?.content) ? latest.content : [];
      const text = content.map((item) => item?.text ?? "").join("").trim();
      const latestUserIndex = input.findLastIndex((item) => (
        item?.type === "message" && item.role === "user"
      ));
      const activeInput = latestUserIndex < 0 ? input : input.slice(latestUserIndex);
      const toolOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-web"
      ));
      const imageOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-image"
      ));
      const computerOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "computer-runtime"
      ));
      const multiplayerConnectorOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "multiplayer-no-connectors"
      ));
      const hostedToolsOutput = input.find((item) => (
        item?.type === "custom_tool_call_output" && item.call_id === "managed-hosted-exec"
      ));
      const hostedPriorityOutput = activeInput.slice().reverse().find((item) => (
        item?.type === "custom_tool_call_output"
        && (item.call_id === "hosted-priority-local" || item.call_id === "hosted-priority-cloud")
      ));
      const phoneOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-phone"
      ));
      const spawnOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-spawn"
      ));
      const waitOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-wait"
      ));
      const submitOutput = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-submit"
      ));
      const managedMemoryFind = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-memory-find"
      ));
      const managedMemoryRead = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "managed-memory-read"
      ));
      const atomicStoreScan = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "atomic-store-scan"
      ));
      const atomicStorePut = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "atomic-store-put"
      ));
      const atomicRecallScan = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "atomic-recall-scan"
      ));
      const atomicRecallRead = input.find((item) => (
        item?.type === "function_call_output" && item.call_id === "atomic-recall-read"
      ));
      pendingResponse = setTimeout(() => {
        pendingResponse = undefined;
        if (hostedPriorityOutput) {
          const local = hostedPriorityOutput.call_id === "hosted-priority-local";
          const output = JSON.stringify(hostedPriorityOutput.output);
          const valid = local
            ? output.includes("private-local")
            : output.includes("ROUTING_COLLISION_EXEC") && !output.includes("PRIVATE_LOCAL_EXEC");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{
                  type: "output_text",
                  text: valid
                    ? (local ? "HOSTED_LOCAL_PRIORITY_OK" : "HOSTED_CLOUD_FALLBACK_OK")
                    : "HOSTED_PRIORITY_BAD",
                }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (hostedToolsOutput) {
          const valid = JSON.stringify(hostedToolsOutput.output).includes("private-host");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{
                  type: "output_text",
                  text: valid ? "MANAGED_HOSTED_TOOLS_OK" : "MANAGED_HOSTED_TOOLS_BAD",
                }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (submitOutput) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: "submitted" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (waitOutput) {
          const valid = String(waitOutput.output).includes('"state":"completed"')
            && String(waitOutput.output).includes('"report":"MANAGED_SUBAGENT_CHILD_OK"');
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{
                  type: "output_text",
                  text: valid ? "MANAGED_SUBAGENTS_OK" : "MANAGED_SUBAGENTS_BAD",
                }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (spawnOutput) {
          let agentId;
          try { agentId = JSON.parse(String(spawnOutput.output)).agent_id; } catch {}
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-wait",
                name: "wait_agent",
                arguments: JSON.stringify({ agent_ids: [agentId], timeout_ms: 30_000 }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (phoneOutput) {
          const output = String(phoneOutput.output);
          const valid = output.includes('"ok":true')
            && output.includes('"status":"completed"')
            && output.includes('"operation":"device.location.current"')
            && output.includes('"latitude":37.8715');
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: valid ? "MANAGED_PHONE_OK" : "MANAGED_PHONE_BAD" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (multiplayerConnectorOutput) {
          const output = String(multiplayerConnectorOutput.output);
          const valid = output.includes("MULTIPLAYER_RUNTIME_OK")
            && output.includes("requires_login")
            && !output.includes("gakonst/nanocodex");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{
                  type: "output_text",
                  text: valid ? "MULTIPLAYER_CONNECTORS_BLOCKED" : "MULTIPLAYER_CONNECTORS_EXPOSED",
                }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (computerOutput) {
          const valid = String(computerOutput.output).includes("COMPUTER_RUNTIME_OK")
            && String(computerOutput.output).includes("gakonst/nanocodex");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: valid ? "COMPUTER_TOOLS_OK" : "COMPUTER_TOOLS_BAD" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (atomicRecallRead) {
          const valid = String(atomicRecallRead.output).includes("Production deploys happen on Tuesdays.");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: valid ? "ATOMIC_MEMORY_RECALLED" : "ATOMIC_MEMORY_BAD" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (atomicRecallScan) {
          let scanned;
          try { scanned = JSON.parse(String(atomicRecallScan.output)); } catch {}
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "atomic-recall-read",
                name: "memory",
                arguments: JSON.stringify({
                  operation: "read",
                  keys: scanned?.candidates?.[0]?.key ? [scanned.candidates[0].key] : [],
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (atomicStorePut) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: "ATOMIC_MEMORY_STORED" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (atomicStoreScan) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "atomic-store-put",
                name: "memory",
                arguments: JSON.stringify({
                  operation: "put",
                  content: "Production deploys happen on Tuesdays.",
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (managedMemoryRead) {
          const valid = String(managedMemoryRead.output).includes("COPPER_LIGHTHOUSE_MEMORY");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{
                  type: "output_text",
                  text: valid ? "MANAGED_MEMORY_TOOLS_OK" : "MANAGED_MEMORY_TOOLS_BAD",
                }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (managedMemoryFind) {
          let found;
          try { found = JSON.parse(String(managedMemoryFind.output)); } catch {}
          const hit = found?.sessions?.[0];
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-memory-read",
                name: "read_session",
                arguments: JSON.stringify({
                  session_id: hit?.session_id,
                  turn_ids: hit?.turn_id ? [hit.turn_id] : [],
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (toolOutput && imageOutput) {
          const valid = String(toolOutput.output).includes("MANAGED_WEB_SEARCH_OK")
            && String(imageOutput.output).includes("TUFOQUdFRF9JTUFHRV9PSw==");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "message",
                role: "assistant",
                content: [{ type: "output_text", text: valid ? "MANAGED_WEB_OK" : "MANAGED_WEB_BAD" }],
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MANAGED_WEB")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-web",
                name: "web__run",
                arguments: JSON.stringify({ search_query: [{ q: "managed web" }] }),
              }, {
                type: "function_call",
                call_id: "managed-image",
                name: "image_gen__imagegen",
                arguments: JSON.stringify({ prompt: "draw managed" }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_HOSTED_TOOLS")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "custom_tool_call",
                call_id: "managed-hosted-exec",
                name: "exec",
                input: "text(await tools.fixture__lookup({ id: 'item-1' }));",
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_HOSTED_PRIORITY_LOCAL")
          || text.includes("E2E_HOSTED_PRIORITY_CLOUD")) {
          const local = text.includes("E2E_HOSTED_PRIORITY_LOCAL");
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "custom_tool_call",
                call_id: local ? "hosted-priority-local" : "hosted-priority-cloud",
                name: "exec",
                input: "text(await tools.exec_command({ cmd: 'printf ROUTING_COLLISION_EXEC' }));",
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_COMPUTER_RUNTIME")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "computer-runtime",
                name: "exec_command",
                arguments: JSON.stringify({
                  cmd: "printf 'COMPUTER_RUNTIME_OK\\n' > /workspace/computer.txt && cat /workspace/computer.txt && gh api repos/gakonst/nanocodex | jq -r .full_name",
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MANAGED_SUBAGENTS")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-spawn",
                name: "spawn_agent",
                arguments: JSON.stringify({
                  role: "managed-reviewer",
                  task: "Return MANAGED_SUBAGENT_CHILD_OK. Marker: E2E_MANAGED_SUBAGENT_CHILD",
                  output_schema: {
                    type: "object",
                    properties: { report: { type: "string" } },
                    required: ["report"],
                    additionalProperties: false,
                  },
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MANAGED_SUBAGENT_CHILD")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-submit",
                name: "submit_result",
                arguments: JSON.stringify({
                  turn_token: 1,
                  output: { report: "MANAGED_SUBAGENT_CHILD_OK" },
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MULTIPLAYER_NO_CONNECTORS")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "multiplayer-no-connectors",
                name: "exec_command",
                arguments: JSON.stringify({
                  cmd: "printf 'MULTIPLAYER_RUNTIME_OK\\n' > /workspace/multiplayer.txt; cat /workspace/multiplayer.txt; gh api repos/gakonst/nanocodex",
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MANAGED_PHONE")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-phone",
                name: "phone",
                arguments: JSON.stringify({
                  operation: "device.location.current",
                  arguments: { provider: "precise" },
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_MEMORY_TOOL")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "managed-memory-find",
                name: "find_sessions",
                arguments: JSON.stringify({
                  query: "copper lighthouse",
                  limit: 8,
                }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_ATOMIC_MEMORY_REMEMBER")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "atomic-store-scan",
                name: "memory",
                arguments: JSON.stringify({ operation: "scan", query: "production deploy schedule" }),
              }],
              usage: null,
            },
          }));
          return;
        }
        if (text.includes("E2E_ATOMIC_MEMORY_RECALL")) {
          server.send(JSON.stringify({
            type: "response.completed",
            response: {
              id: crypto.randomUUID(),
              status: "completed",
              output: [{
                type: "function_call",
                call_id: "atomic-recall-scan",
                name: "memory",
                arguments: JSON.stringify({ operation: "scan", query: "Tuesday production deploy" }),
              }],
              usage: null,
            },
          }));
          return;
        }
        server.send(JSON.stringify({
          type: "response.completed",
          response: {
            id: crypto.randomUUID(),
            status: "completed",
            output: [{
              type: "message",
              role: "assistant",
              content: [{ type: "output_text", text: "ROOM_AGENT_OK: " + text.slice(-160) }],
            }],
            usage: null,
          },
        }));
      }, 500);
    });
    server.addEventListener("close", () => {
      if (pendingResponse !== undefined) clearTimeout(pendingResponse);
    });
    return new Response(null, {
      status: 101,
      webSocket: client,
      headers: { "openai-model": "test-model", "x-request-id": crypto.randomUUID() },
    });
  },
};
`;

export default defineConfig({
  plugins: [
    cloudflareTest({
      // Unit/integration tests exercise the local lexical fallback. The hosted
      // smoke suite owns the real, billable AI Search binding.
      wrangler: { configPath: "./wrangler.test.jsonc" },
      miniflare: {
        bindings: {
          AGENT_IDLE_TIMEOUT_MS: "1000",
          NANOCODEX_ADMIN_TOKEN: "test-admin-token",
          NANOCODEX_ROOM_ALLOCATOR_TOKEN: "test-room-allocator-token",
        },
        workers: [{
          name: "nanocodex-egress",
          modules: true,
          script: TEST_BROKER,
        }],
      },
    }),
  ],
  test: {
    include: ["test/**/*.test.ts"],
    testTimeout: 15_000,
  },
});
