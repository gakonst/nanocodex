import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { test } from "node:test";
import { Challenge, Credential, Mcp, Method } from "mppx";
import { Methods } from "mppx/tempo";
import { createMcpRuntime } from "../runtime/mcp-runtime.mjs";
import { mcpPayment } from "../tempo/index.mjs";

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

// Synthetic remote MCP over the SDK's real Streamable HTTP transport. No wallet
// or external service is needed: the challenge and credential are test fixtures.
async function remote(t) {
  const calls = [];
  const challenge = Challenge.from({
    id: "synthetic-lazy-mcp", intent: "charge", method: "tempo",
    realm: "synthetic.example", request: {},
  });
  const server = createServer(async (request, response) => {
    if (request.method !== "POST") { response.writeHead(405).end(); return; }
    let body = "";
    for await (const part of request) body += part;
    const message = JSON.parse(body);
    calls.push(message);
    if (message.id === undefined) { response.writeHead(202).end(); return; }
    let result;
    if (message.method === "initialize") {
      result = { protocolVersion: message.params.protocolVersion, capabilities: { tools: {} },
        serverInfo: { name: "synthetic", version: "1" } };
    } else if (message.method === "tools/list") {
      result = { tools: [{ name: "premium", description: "Synthetic premium lookup",
        inputSchema: { type: "object" } }] };
    } else if (message.method === "tools/call") {
      result = message.params._meta?.[Mcp.credentialMetaKey]
        ? { content: [{ type: "text", text: "paid result" }] }
        : { content: [], _meta: { [Mcp.paymentRequiredMetaKey]: { challenges: [challenge] } } };
    } else {
      response.writeHead(400).end(); return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise((resolve) => server.close(resolve)));
  return { url: `http://127.0.0.1:${server.address().port}/mcp`, calls };
}

function paymentOptions(onCredential = () => {}) {
  return { methods: [Method.toClient(Methods.charge, {
    async createCredential({ challenge }) {
      onCredential();
      return Credential.serialize({ challenge, payload: { signature: "0xsynthetic", type: "transaction" } });
    },
  })] };
}

async function runtime(t, server, payment, options = {}) {
  const mcp = await createMcpRuntime({ synthetic: { url: server.url, payment, ...options } });
  t.after(() => mcp.close());
  await mcp.settled();
  const handler = mcp.resolve("mcp__synthetic__premium")?.handler;
  assert.equal(typeof handler, "function", JSON.stringify(mcp.search({ query: "premium" })));
  return { mcp, handler };
}

test("HTTP discovery leaves lazy payment untouched; concurrent first tools share setup and pay normally", async (t) => {
  const server = await remote(t);
  const gate = deferred();
  const started = deferred();
  let factories = 0;
  let credentials = 0;
  let approvals = 0;
  const payment = mcpPayment(async () => {
    factories++;
    started.resolve();
    await gate.promise;
    return { ...paymentOptions(() => credentials++), onPaymentRequired() { approvals++; return true; } };
  });
  assert.equal(mcpPayment(payment), payment);
  const { mcp, handler } = await runtime(t, server, payment);
  mcp.definitions();
  assert.equal(mcp.search({ query: "premium" }).value.tools.length, 1);
  assert.equal(factories, 0);
  assert.ok(server.calls.some((call) => call.method === "tools/list"));
  const first = handler({ key: "first" });
  const second = handler({ key: "second" });
  await started.promise;
  assert.equal(factories, 1);
  assert.equal(server.calls.filter((call) => call.method === "tools/call").length, 0);
  gate.resolve();
  for (const result of await Promise.all([first, second, handler({ key: "third" })])) {
    assert.equal(result.value.content[0].text, "paid result");
  }
  assert.equal(factories, 1);
  assert.equal(credentials, 3);
  assert.equal(approvals, 3);
  assert.equal(server.calls.filter((call) => call.method === "tools/call").length, 6);
  t.diagnostic("discovery: factories=0; three calls: factories=1, approvals=3, credentials=3, remote requests=6");
});

test("cancelling a tool during shared setup does not dispatch it or poison another caller", async (t) => {
  const server = await remote(t);
  const gate = deferred();
  const started = deferred();
  let factories = 0;
  const { handler } = await runtime(t, server, mcpPayment(async () => {
    factories++;
    started.resolve();
    await gate.promise;
    return paymentOptions();
  }));
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(handler({ key: "pre-cancelled" }, { signal: controller.signal }), /cancelled/);
  assert.equal(factories, 0);
  const pending = new AbortController();
  const cancelled = handler({ key: "cancelled" }, { signal: pending.signal });
  const rejected = assert.rejects(cancelled, /cancelled/);
  await started.promise;
  const live = handler({ key: "live" });
  pending.abort();
  await rejected;
  gate.resolve();
  assert.equal((await live).value.content[0].text, "paid result");
  assert.equal(factories, 1);
  assert.deepEqual(server.calls.filter((call) => call.method === "tools/call")
    .map((call) => call.params.arguments.key), ["live", "live"]);
  t.diagnostic("pre-cancelled call did not start factory; cancelled setup waiter sent no request; live waiter paid once");
});

test("setup failure remains cached and sends no MCP tool or payment request", async (t) => {
  const server = await remote(t);
  let factories = 0;
  const { handler } = await runtime(t, server, mcpPayment(async () => {
    factories++;
    throw new Error("synthetic setup failed");
  }));
  for (let i = 0; i < 2; i++) await assert.rejects(handler({}), /synthetic setup failed/);
  assert.equal(factories, 1);
  assert.equal(server.calls.filter((call) => call.method === "tools/call").length, 0);
  t.diagnostic("two tool attempts: one failed factory invocation, zero remote tool requests");
});

test("revoked availability after lazy setup prevents dispatch", async (t) => {
  const server = await remote(t);
  const gate = deferred();
  const started = deferred();
  let available = true;
  const { handler } = await runtime(t, server, mcpPayment(async () => {
    started.resolve();
    await gate.promise;
    return paymentOptions();
  }), { isAvailable: () => available });
  const call = handler({});
  const rejected = assert.rejects(call, /unavailable/);
  await started.promise;
  available = false;
  gate.resolve();
  await rejected;
  assert.equal(server.calls.filter((call) => call.method === "tools/call").length, 0);
  t.diagnostic("access revoked while factory waited: zero remote tool requests");
});

test("a setup deadline prevents late dispatch while a later caller can use the shared result", async (t) => {
  const server = await remote(t);
  const gate = deferred();
  let factories = 0;
  const { handler } = await runtime(t, server, mcpPayment(async () => {
    factories++;
    await gate.promise;
    return paymentOptions();
  }), { timeoutMs: 100 });
  await assert.rejects(handler({ key: "expired" }), /exceeded 100 milliseconds/);
  const live = handler({ key: "live" });
  gate.resolve();
  assert.equal((await live).value.content[0].text, "paid result");
  assert.equal(factories, 1);
  assert.deepEqual(server.calls.filter((call) => call.method === "tools/call")
    .map((call) => call.params.arguments.key), ["live", "live"]);
  t.diagnostic("expired setup waiter sent no request after setup completed; later caller reused the sole factory result");
});

test("lazy payment preserves approval refusal without signing or retrying", async (t) => {
  const server = await remote(t);
  let credentials = 0;
  let approvals = 0;
  const { handler } = await runtime(t, server, mcpPayment(() => ({
    ...paymentOptions(() => credentials++),
    onPaymentRequired() { approvals++; return false; },
  })));
  await assert.rejects(handler({}), /Payment declined/);
  assert.equal(approvals, 1);
  assert.equal(credentials, 0);
  assert.equal(server.calls.filter((call) => call.method === "tools/call").length, 1);
  t.diagnostic("declined approval: one challenge, zero credentials, zero retries");
});
