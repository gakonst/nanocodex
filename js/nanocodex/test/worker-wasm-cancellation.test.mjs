import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { test } from "node:test";

import { Transport } from "../browser/index.mjs";
import { createWorkerAgent } from "../browser/WorkerAgent.mjs";
import { NodeWebWorker } from "./support/node-web-worker.mjs";
import {
  deferred,
  messageReader,
  sendCompleted,
  sendFinal,
  sendWarmup,
  startResponsesServer,
} from "./support/responses.mjs";

const WASM_URL = new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url);
const AGENT_WORKER_URL = new URL("../browser/agent.worker.mjs", import.meta.url);
const TOOL_AGENT_WORKER_URL = new URL("./support/tool-agent.worker.mjs", import.meta.url);

for (const [label, source] of [
  ["CPU loop", "while (true) {}"],
  ["never-settling promise", "await new Promise(() => {});"],
]) {
  test(`compiled Worker Agent cancels a ${label} and remains live`, async () => {
    const server = await startResponsesServer();
    const wasm = await readFile(WASM_URL);
    const worker = new NodeWebWorker(AGENT_WORKER_URL, { name: `agent-${label}` });
    const agent = await createWorkerAgent({
      harness: false,
      module: wasm,
      sessionId: label === "CPU loop"
        ? "018f1f9a-7b3c-7a07-8000-000000000011"
        : "018f1f9a-7b3c-7a07-8000-000000000012",
      thinking: "low",
      transport: Transport.openAi({
        apiKey: "test-key",
        websocketUrl: server.url,
        websocketWarmup: true,
      }),
    }, { worker });
    const started = deferred();
    const watch = agent.events.watch();
    watch.onEvent((event) => {
      if (event.type === "tool.call" && event.payload.call_id === "call-wedged") {
        started.resolve();
      }
    });

    try {
      const turn = agent.turn.prompt({ input: `Run the ${label}.` });
      const socket = await within(server.nextConnection(), 2_000, "initial connection");
      const reader = messageReader(socket);
      await within(reader.next(), 2_000, "warmup request");
      sendWarmup(socket, `warmup-${label}`);
      await within(reader.next(), 2_000, "generation request");
      sendCompleted(socket, `exec-${label}`, [{
        type: "custom_tool_call",
        call_id: "call-wedged",
        name: "exec",
        input: source,
      }]);

      const result = turn.result();
      void result.catch(() => {});
      await within(started.promise, 2_000, "Code Mode tool start");
      await within(turn.cancel(), 1_000, "turn cancellation");
      await assert.rejects(
        within(result, 1_000, "cancelled turn result"),
        /cancel/i,
      );

      const followOn = agent.turn.prompt({ input: "Reply RECOVERED." });
      const replacement = await within(server.nextConnection(), 2_000, "replacement connection");
      const replacementReader = messageReader(replacement);
      const replay = await within(replacementReader.next(), 2_000, "follow-on request");
      assert.equal(replay.previous_response_id, undefined);
      assert.match(JSON.stringify(replay.input), /Run the/);
      assert.match(JSON.stringify(replay.input), /Reply RECOVERED/);
      sendFinal(replacement, `recovered-${label}`, "RECOVERED");
      const recovered = await within(followOn.result(), 2_000, "follow-on result");
      assert.equal(recovered.finalMessage, "RECOVERED");
      recovered.dispose();
    } finally {
      watch.off();
      agent.dispose();
      for (const socket of server.websocketServer.clients) socket.terminate();
      await server.close();
    }
  });
}

test("compiled Worker Agent aborts active nested work and reuses the tool runtime", async () => {
  const server = await startResponsesServer();
  const blocker = await startBlockingServer();
  const wasm = await readFile(WASM_URL);
  const worker = new NodeWebWorker(TOOL_AGENT_WORKER_URL, { name: "agent-nested-cancel" });
  const agent = await createWorkerAgent({
    blockedUrl: blocker.url,
    harness: false,
    module: wasm,
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000013",
    thinking: "low",
    transport: Transport.openAi({
      apiKey: "test-key",
      websocketUrl: server.url,
      websocketWarmup: true,
    }),
  }, { worker });
  const events = [];
  const runCancelled = deferred();
  const recoveredNestedCompleted = deferred();
  const watch = agent.events.watch();
  watch.onEvent((event) => {
    events.push(event);
    if (event.type === "run.failed" && event.payload.status === "cancelled") {
      runCancelled.resolve();
    } else if (
      event.type === "tool.result"
      && event.payload.call_id.startsWith("call-nested-recovered/code-")
    ) {
      recoveredNestedCompleted.resolve();
    }
  });

  try {
    const turn = agent.turn.prompt({ input: "Run the blocking nested tool." });
    const socket = await within(server.nextConnection(), 2_000, "initial connection");
    const reader = messageReader(socket);
    await within(reader.next(), 2_000, "warmup request");
    sendWarmup(socket, "nested-warmup");
    await within(reader.next(), 2_000, "generation request");
    sendCompleted(socket, "nested-exec", [{
      type: "custom_tool_call",
      call_id: "call-nested",
      name: "exec",
      input: 'await tools.blocked({ mode: "slow" });',
    }]);

    const result = turn.result();
    void result.catch(() => {});
    await within(blocker.started.promise, 2_000, "nested request start");
    await within(turn.cancel(), 1_000, "nested turn cancellation");
    await within(blocker.aborted.promise, 1_000, "nested request abort");
    await assert.rejects(within(result, 1_000, "cancelled nested result"), /cancel/i);
    await within(runCancelled.promise, 1_000, "cancelled run terminal");

    const cancelledNested = events.filter((event) => (
      event.type === "tool.result"
      && event.payload.call_id === "call-nested/code-1"
    ));
    assert.equal(cancelledNested.length, 1);
    assert.equal(cancelledNested[0].payload.tool, "blocked");
    assert.equal(cancelledNested[0].payload.status, "cancelled");
    assert.match(cancelledNested[0].payload.result, /^aborted by user after /);
    assert.equal(
      cancelledNested[0].payload.structured_result,
      cancelledNested[0].payload.result,
    );
    assert.equal(Number.isSafeInteger(cancelledNested[0].payload.duration_ns), true);
    assert.equal(Number.isSafeInteger(cancelledNested[0].payload.started_after_ns), true);
    const nestedTerminalIndex = events.indexOf(cancelledNested[0]);
    const outerTerminalIndex = events.findIndex((event) => (
      event.type === "tool.result"
      && event.payload.call_id === "call-nested"
      && event.payload.status === "cancelled"
    ));
    const runTerminalIndex = events.findIndex((event) => (
      event.type === "run.failed" && event.payload.status === "cancelled"
    ));
    assert(nestedTerminalIndex < outerTerminalIndex);
    assert(outerTerminalIndex < runTerminalIndex);

    const followOn = agent.turn.prompt({ input: "Run the fast nested tool." });
    const replacement = await within(server.nextConnection(), 2_000, "replacement connection");
    const replacementReader = messageReader(replacement);
    await within(replacementReader.next(), 2_000, "follow-on request");
    sendCompleted(replacement, "nested-recovered-exec", [{
      type: "custom_tool_call",
      call_id: "call-nested-recovered",
      name: "exec",
      input: 'text(await tools.blocked({ mode: "fast" }));',
    }]);
    const continuation = await within(replacementReader.next(), 2_000, "tool continuation");
    assert.match(JSON.stringify(continuation.input), /RECOVERED_TOOL/);
    sendFinal(replacement, "nested-recovered", "RECOVERED");
    const recovered = await within(followOn.result(), 2_000, "follow-on result");
    assert.equal(recovered.finalMessage, "RECOVERED");
    await within(recoveredNestedCompleted.promise, 1_000, "recovered nested terminal");
    const recoveredNested = events.filter((event) => (
      event.type === "tool.result"
      && event.payload.call_id.startsWith("call-nested-recovered/code-")
    ));
    assert.equal(recoveredNested.length, 1);
    assert.equal(recoveredNested[0].payload.tool, "blocked");
    assert.equal(recoveredNested[0].payload.status, "completed");
    assert(events.indexOf(recoveredNested[0]) > runTerminalIndex);
    recovered.dispose();
  } finally {
    watch.off();
    agent.dispose();
    for (const socket of server.websocketServer.clients) socket.terminate();
    await Promise.all([server.close(), blocker.close()]);
  }
});

function within(promise, milliseconds, label) {
  let timer;
  return Promise.race([
    Promise.resolve(promise),
    new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(new Error(`${label} exceeded ${milliseconds} milliseconds`)),
        milliseconds,
      );
    }),
  ]).finally(() => clearTimeout(timer));
}

async function startBlockingServer() {
  const started = deferred();
  const aborted = deferred();
  const sockets = new Set();
  const server = createServer((_request, response) => {
    started.resolve();
    response.once("close", () => {
      if (!response.writableEnded) aborted.resolve();
    });
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
    server.listen(0, "127.0.0.1");
  });
  return {
    aborted,
    started,
    url: `http://127.0.0.1:${server.address().port}/blocked`,
    close() {
      for (const socket of sockets) socket.destroy();
      return new Promise((resolve, reject) => {
        server.close((error) => error ? reject(error) : resolve());
      });
    },
  };
}
