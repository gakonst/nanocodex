import assert from "node:assert/strict";
import test from "node:test";
import { provider } from "./provider-fixture.mjs";

import { HostedToolsBrokerCore } from "nanocodex-tools/hosted";
import { createTools } from "../../nanocodex/tools/Tools.mjs";
import { connectComputerTools } from "../index.mjs";
import { CUA_JS_NAME, CUA_RESET_NAME } from "../contract.mjs";

test("provider screenshots cross MCP attachment, broker, and model output", { timeout: 30_000 }, async t => {
  const computer = await connectComputerTools(provider());
  const tools = await createTools({
    tools: Object.fromEntries(computer.tools.map(tool => [tool.name, tool])),
  });
  const persistence = new MemoryPersistence();
  const attachments = new WeakMap();
  const sockets = [];
  let sequence = 0;
  const context = {
    accept() {},
    sockets: () => sockets.filter(socket => socket.readyState === 1),
    readAttachment: socket => attachments.get(socket),
    writeAttachment: (socket, value) => attachments.set(socket, structuredClone(value)),
  };
  const broker = new HostedToolsBrokerCore(context, {
    persistence,
    randomUUID: () => `00000000-0000-4000-8000-${String(++sequence).padStart(12, "0")}`,
  });
  const server = new BrokerSocket();
  const client = new AttachmentSocket(server, broker);
  server.client = client;
  sockets.push(server);
  broker.accept(server, "attachment:fixture");
  const connector = tools.attach({
    endpoint: "wss://managed.test/tools",
    transport: { connect: async () => client },
  });
  const connected = await connector.connect();
  t.after(async () => {
    await connected.close();
    await tools.close();
    await computer.close();
  });

  const tool = broker.provider().resolve(CUA_JS_NAME);
  const reset = broker.provider().resolve(CUA_RESET_NAME);
  assert(tool, "hosted CUA tool was not published");
  assert(reset, "hosted CUA reset tool was not published");
  assert.equal(tool.parallelSafe, true);
  assert.equal(reset.parallelSafe, true);
  const sessions = Array.from({ length: 4 }, (_, index) => `conversation:${index}`);
  await Promise.all(sessions.map((sessionId, index) => tool.handler(
    { set: "selected" },
    { sessionId, callId: `select:${index}`, model: "gpt-6-sol" },
  )));
  const results = await Promise.all(sessions.map((sessionId, index) => tool.handler(
    { image: true },
    { sessionId, callId: `shot:${index}`, model: "gpt-6-sol" },
  )));
  for (const result of results) assertPng(result);
  await reset.handler({}, { sessionId: sessions[0], callId: "reset:0", model: "gpt-6-sol" });
  const cleared = await tool.handler(
    { get: true },
    { sessionId: sessions[0], callId: "after-reset:0", model: "gpt-6-sol" },
  );
  assert.equal(cleared.output.at(-1).text, "undefined");
});

function assertPng(result) {
  const image = result.output.find(item => item.type === "input_image");
  assert(image?.image_url.startsWith("data:image/png;base64,"));
  assert.equal(image.detail, "original");
  const png = Buffer.from(image.image_url.split(",", 2)[1], "base64");
  assert.deepEqual([...png.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
}

class AttachmentSocket {
  readyState = 1;
  listeners = new Map();
  constructor(server, broker) { this.server = server; this.broker = broker; }
  send(data) { void this.broker.webSocketMessage(this.server, data); }
  close(code = 1000, reason = "") {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.emit("close", { code, reason });
    this.broker.webSocketClose(this.server, code, reason);
  }
  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }
  receive(data) { this.emit("message", { data }); }
  remoteClose(code, reason) {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.emit("close", { code, reason });
  }
  emit(type, event) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

class BrokerSocket {
  readyState = 1;
  client;
  send(data) { this.client.receive(data); }
  close(code = 1000, reason = "") {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.client.remoteClose(code, reason);
  }
}

class MemoryPersistence {
  routes = new Map();
  calls = new Map();
  initialize() { return []; }
  transaction(callback) { return callback(); }
  states() { return [...this.routes.values()].map(clone); }
  state(routeId) { return clone(this.routes.get(routeId)); }
  replaceHost(row) { this.routes.set(row.route_id, clone(row)); }
  clearHost(leaseId, generation) {
    const row = [...this.routes.values()].find(value => value.lease_id === leaseId && value.generation === generation);
    if (row) this.routes.set(row.route_id, { ...row, host_id: null, lease_id: null, lease_expires_at: 0 });
  }
  clearCatalog(leaseId, generation) {
    const row = [...this.routes.values()].find(value => value.lease_id === leaseId && value.generation === generation);
    if (row) this.routes.set(row.route_id, { ...row, catalog_json: null, machines_json: null });
  }
  call(callId) { return clone(this.calls.get(callId)); }
  callBySource(sessionId, sourceCallId) {
    return clone([...this.calls.values()].find(row => row.session_id === sessionId && row.source_call_id === sourceCallId));
  }
  insertCall(row) {
    if (this.calls.has(row.call_id) || this.callBySource(row.session_id, row.source_call_id)) throw new Error("duplicate call");
    this.calls.set(row.call_id, clone(row));
  }
  markCancelRequested(callId) {
    const row = this.calls.get(callId);
    if (row?.state === "dispatched") row.cancel_requested = 1;
    return clone(row);
  }
  transitionCall(callId, from, state, resultJson) {
    const row = this.calls.get(callId);
    if (row && from.includes(row.state)) {
      row.state = state;
      row.result_json = resultJson || null;
    }
    return clone(row);
  }
  recordLateReceipt(callId, receiptJson) {
    const row = this.calls.get(callId);
    if (row?.state === "ambiguous" && row.receipt_json === null) row.receipt_json = receiptJson;
    return clone(row);
  }
  markGenerationAmbiguous(leaseId, generation, resultJson) {
    for (const row of this.calls.values()) {
      if (row.lease_id === leaseId && row.generation === generation && row.state === "dispatched") {
        row.state = "ambiguous";
        row.result_json = resultJson;
      }
    }
  }
  activeCallCount(leaseId, generation) {
    return [...this.calls.values()].filter(row => row.lease_id === leaseId && row.generation === generation
      && (row.state === "admitted" || row.state === "dispatched")).length;
  }
  generationCallCount(leaseId, generation) {
    return [...this.calls.values()].filter(row => row.lease_id === leaseId && row.generation === generation).length;
  }
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}
