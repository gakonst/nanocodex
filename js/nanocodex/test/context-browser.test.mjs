import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { open } from "../node/workspace.mjs";
import { test } from "node:test";
import WebSocket, { WebSocketServer } from "ws";
import { create } from "../browser/InlineAgent.mjs";
import * as Transport from "../browser/Transport.mjs";

for (const enabled of [false, true]) test(`browser WASM uses the supplied context workspace: ${enabled}`, async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve) => server.once("listening", resolve));
  const connection = new Promise((resolve) => server.once("connection", (socket) => {
    socket.send(JSON.stringify({ type: "nanocodex.proxy.ready" }));
    resolve(socket);
  }));
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-browser-context-"));
  const agent = await create({
    contextStorage: enabled ? await open({ path: directory }) : undefined,
    module: await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)),
    transport: Transport.hostManaged({
      apiBaseUrl: "https://nanocodex.internal/v1",
      websocketUrl: `ws://127.0.0.1:${server.address().port}`,
      WebSocketImpl: WebSocket,
      websocketPreconnect: false,
      websocketWarmup: true,
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
  } finally {
    await agent.session.shutdown();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true });
  }
});
