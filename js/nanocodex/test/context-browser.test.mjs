import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import WebSocket, { WebSocketServer } from "ws";
import { create } from "../browser/InlineAgent.mjs";
import * as Transport from "../browser/Transport.mjs";

for (const enabled of [false, true]) test(`browser WASM uses private host context capability: ${enabled}`, async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve) => server.once("listening", resolve));
  const connection = new Promise((resolve) => server.once("connection", (socket) => {
    socket.send(JSON.stringify({ type: "nanocodex.proxy.ready" }));
    resolve(socket);
  }));
  const calls = [];
  const agent = await create({
    module: await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)),
    transport: Transport.hostManaged({
      apiBaseUrl: "https://nanocodex.internal/v1",
      websocketUrl: `ws://127.0.0.1:${server.address().port}`,
      WebSocketImpl: WebSocket,
      websocketPreconnect: false,
      websocketWarmup: true,
      historyNotes: {
        async available() { return enabled; },
        async request(input) {
          calls.push(input);
          return Response.json({ text: "Saved notes are available." });
        },
      },
    }),
  });
  try {
    const scenario = (async () => {
      const socket = await connection;
      const queue = [];
      let pending;
      socket.on("message", (data) => {
        const frame = JSON.parse(String(data));
        if (pending) { const receive = pending; pending = undefined; receive(frame); }
        else queue.push(frame);
      });
      const next = () => queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve) => { pending = resolve; });
      const send = (id, output = []) => socket.send(JSON.stringify({ type: "response.completed", response: { id, status: "completed", output } }));
      const warmup = await next();
      assert.equal(warmup.model, "gpt-6-astra");
      assert.equal(warmup.input[0].tools.some((tool) => tool.name === "new_context"), enabled);
      send("warmup");
      const first = await next();
      if (enabled) {
        const metadata = JSON.parse(first.client_metadata["x-codex-turn-metadata"]);
        send("reset", [{ type: "function_call", name: "new_context", call_id: "reset-context", arguments: "{}" }]);
        const reset = await next();
        const nextMetadata = JSON.parse(reset.client_metadata["x-codex-turn-metadata"]);
        assert.equal(nextMetadata.window_number, 1);
        assert.notEqual(nextMetadata.context_window_id, metadata.context_window_id);
        assert.equal(reset.prompt_cache_key, first.prompt_cache_key);
      }
      send("final", [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "done" }] }]);
    })();
    const [result] = await Promise.all([agent.turn.prompt({ input: "Start a fresh context." }).result(), scenario]);
    assert.equal(result.finalMessage, "done");
    assert.equal(calls.length, enabled ? 2 : 0);
    for (const call of calls) {
      assert.equal(call.path, "alpha/notes/v2/thread_hint");
      assert.equal(call.body.context.session_id, agent.sessionId);
      assert.equal(call.body.context.current_agent_name, "/root");
      assert.equal(call.bearer, undefined);
      assert.equal(call.accountId, undefined);
    }
  } finally {
    await agent.session.shutdown();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve) => server.close(resolve));
  }
});
