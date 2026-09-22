// Local combined routing/runtime test (Node >=24):
// node --test test/thread-routing-wasm.test.mjs
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import { resolveThreadRoute, routingPolicySchema, ThreadRoutePin, OSS_MODEL } from "../src/thread-model-routing.ts";
import { initializeManagedAgentSettingsSchema } from "../src/agent-settings-schema.ts";
import { Agent, Transport } from "../../nanocodex/host/index.mjs";
import { createWorkersAiResponses } from "../../nanocodex/cloudflare/workers-ai-responses.mjs";

// Uses real SQLite with the production settings schema. The store's transaction
// mirrors DurableAgentSession; HTTP admission is covered by workerd tests.
async function fixture(run) {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-routing-"));
  let db;
  const open = () => {
    db = new DatabaseSync(join(directory, "route.sqlite"));
    const storage = {
      sql: { exec(sql) {
        if (/^\s*SELECT/.test(sql)) { const rows = db.prepare(sql).all(); return { one: () => rows[0] }; }
        db.exec(sql);
      } },
      transactionSync(fn) { db.exec("BEGIN"); try { fn(); db.exec("COMMIT"); } catch (error) { db.exec("ROLLBACK"); throw error; } },
    };
    initializeManagedAgentSettingsSchema(storage);
    db.exec("CREATE TABLE IF NOT EXISTS managed_thread_route (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), route_json TEXT NOT NULL)");
    return new ThreadRoutePin({
      read() { const row = db.prepare("SELECT route_json FROM managed_thread_route WHERE singleton = 1").get(); return row && JSON.parse(row.route_json); },
      commit(route) { storage.transactionSync(() => {
        db.prepare("INSERT INTO managed_thread_route VALUES (1, ?)").run(JSON.stringify(route));
        db.prepare("UPDATE managed_agent_settings SET model=?, thinking=?, reasoning_mode=?, fast_mode=? WHERE singleton=1")
          .run(route.model, route.thinking, route.reasoning_mode, Number(route.fast_mode));
      }); },
    });
  };
  try { await run({ pin: open(), restart() { db.close(); return open(); }, settings() { return db.prepare("SELECT * FROM managed_agent_settings WHERE singleton=1").get(); } }); }
  finally { db?.close(); await rm(directory, { recursive: true, force: true }); }
}

test("Jev -> committed route -> real WASM GLM tool loop -> second turn retains model/effort without Jev", { timeout: 30_000 }, async () => fixture(async store => {
  const opening = "Call runtimeInfo to inspect this build environment.";
  let jevCalls = 0, glmCalls = 0, executions = 0;
  const binding = { async run(model, input) {
    if (model === "typesafe/jev") {
      jevCalls++;
      assert.equal(JSON.parse(input.state).opening_prompt, opening);
      return { answers: { candidate: { choice: `${OSS_MODEL}:low`, confidence: .99 }, family: { choice: "terminal", confidence: .99 } }, usage: { input_tokens: 20 } };
    }
    assert.equal(model, OSS_MODEL);
    assert.equal(input.reasoning_effort, "low");
    glmCalls++;
    if (glmCalls === 1 || glmCalls === 3) {
      const tool = input.tools.find(tool => tool.function.description.startsWith("runtimeInfo\n"));
      assert.ok(tool, "real Rust tool declaration reaches GLM");
      if (glmCalls === 3) {
        assert.ok(input.messages.some(message => message.content?.includes("TURN_1_OK")), "second turn retains first answer");
        assert.ok(input.messages.some(message => message.content?.includes("Now inspect again")));
      }
      return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
        id: `runtime-${glmCalls}`, type: "function", function: { name: tool.function.name, arguments: "{}" },
      }] } }] };
    }
    assert.ok(glmCalls === 2 || glmCalls === 4);
    const output = input.messages.find(message => message.role === "tool" && message.tool_call_id === `runtime-${glmCalls - 1}`);
    assert.ok(output, "matching actual tool result replayed");
    assert.match(output.content, /local-routing-fixture/);
    return { choices: [{ finish_reason: "stop", message: { content: `TURN_${glmCalls / 2}_OK` } }] };
  } };
  const route = await store.pin.resolve(() => resolveThreadRoute(binding, opening, routingPolicySchema.parse({ preferences: { cost: 80, duration: 15, completion: 5 } })));
  assert.equal(route.backend, "workers_ai");
  assert.equal(route.model, OSS_MODEL);
  assert.equal(route.thinking, "low");
  assert.equal(store.settings().model, route.model);
  assert.equal(store.settings().thinking, route.thinking);
  const transport = createWorkersAiResponses(binding);
  const agent = await Agent.create({
    module: await readFile(new URL("../../nanocodex/pkg-web/nanocodex_bg.wasm", import.meta.url)),
    model: store.settings().model, thinking: store.settings().thinking, toolMode: "direct",
    transport: Transport.hostManaged({ ...transport, websocketPreconnect: false, createWebSocket() { assert.fail("GLM attempted WebSocket"); } }),
    tools: { runtimeInfo: { description: "Return fixture runtime", parameters: { type: "object", additionalProperties: false },
      handler() { executions++; return { runtime: "local-routing-fixture", execution: executions }; },
    } },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: opening }).result()).finalMessage, "TURN_1_OK");
    // Close/reopen SQLite and recreate the pin: no in-memory route can survive.
    const restarted = store.restart();
    const retained = await restarted.resolve(() => resolveThreadRoute(binding, "Now inspect again", routingPolicySchema.parse({ oss_thinking: "high" })));
    assert.deepEqual(retained, route);
    assert.equal(store.settings().model, OSS_MODEL);
    assert.equal(store.settings().thinking, "low");
    assert.equal((await agent.turn.prompt({ input: "Now inspect again" }).result()).finalMessage, "TURN_2_OK");
    assert.equal(jevCalls, 1);
    assert.equal(glmCalls, 4);
    assert.equal(executions, 2);
  } finally { agent.dispose(); }
}));

test("Jev failure persists configured ChatGPT fallback without live credentials", async () => fixture(async store => {
  let jevCalls = 0;
  const ai = { async run(model) { assert.equal(model, "typesafe/jev"); jevCalls++; throw new Error("synthetic provider failure"); } };
  const route = await store.pin.resolve(() => resolveThreadRoute(ai, "Inspect build", routingPolicySchema.parse({ frontier_model: "gpt-5.6-sol", frontier_thinking: "medium" })));
  assert.equal(route.backend, "chatgpt");
  assert.equal(route.selection, "fallback");
  assert.equal(route.model, "gpt-5.6-sol");
  assert.equal(route.thinking, "medium");
  assert.doesNotMatch(JSON.stringify(route), /synthetic provider failure/);
  const retained = await store.restart().resolve(() => { assert.fail("fallback must not reroute"); });
  assert.deepEqual(retained, route);
  assert.equal(store.settings().model, route.model);
  assert.equal(store.settings().thinking, route.thinking);
  assert.equal(jevCalls, 1);
}));

for (const model of ["kimi-k3", "mimo-v2.6-pro"]) test(`${model} real WASM executes a host tool and replays provider reasoning`, { timeout: 30_000 }, async () => {
  const { createGatewayResponses } = await import("../../nanocodex/cloudflare/gateway-responses.mjs");
  let calls = 0, executions = 0;
  const details = [{ type: "reasoning.encrypted", data: "synthetic-replay", index: 0 }];
  const transport = createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "low", apiKey: "synthetic-key",
    fetch: async (_url, init) => {
      const body = JSON.parse(init.body); calls++;
      const reply = (message, finish_reason) => new Response([
        { choices: [{ index: 0, delta: { ...message, ...(message.tool_calls ? { tool_calls: message.tool_calls.map((call, index) => ({ ...call, index })) } : {}) }, finish_reason: null }] },
        { choices: [{ index: 0, delta: {}, finish_reason }] }, "[DONE]",
      ].map(value => `data: ${typeof value === "string" ? value : JSON.stringify(value)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } });
      if (calls === 1) {
        const tool = body.tools.find(t => t.function.description.startsWith("runtimeInfo\n"));
        assert.ok(tool);
        return reply({
          reasoning_details: details, tool_calls: [{ id: "fixture-host-call", type: "function", function: { name: tool.function.name, arguments: "{}" } }],
        }, "tool_calls");
      }
      assert.equal(calls, 2);
      const assistant = body.messages.find(m => m.tool_calls?.length);
      assert.deepEqual(assistant.reasoning_details, details);
      assert.match(body.messages.find(m => m.role === "tool").content, /gateway-host-fixture/);
      return reply({ content: "HOST_TOOL_OK" }, "stop");
    } });
  const agent = await Agent.create({ module: await readFile(new URL("../../nanocodex/pkg-web/nanocodex_bg.wasm", import.meta.url)),
    model, thinking: "low", toolMode: "direct",
    transport: Transport.hostManaged({ ...transport, websocketPreconnect: false, createWebSocket() { assert.fail("gateway attempted account WebSocket"); } }),
    tools: { runtimeInfo: { description: "Inspect fixture", parameters: { type: "object", properties: {} }, handler() { executions++; return { runtime: "gateway-host-fixture" }; } } },
  });
  try {
    assert.equal((await agent.turn.prompt({ input: "Inspect runtime" }).result()).finalMessage, "HOST_TOOL_OK");
    assert.equal(executions, 1); assert.equal(calls, 2);
  } finally { agent.dispose(); }
});
