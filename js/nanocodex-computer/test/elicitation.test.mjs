import test from "node:test";
import assert from "node:assert/strict";
import { connectComputerTools, createComputerTools } from "../index.mjs";

// Shape captured from the current Sky provider's elicitation/create request.
// Fixture app identity is synthetic; approval metadata is deliberately intact.
const params = {
  _meta: {
    codex_approval_kind: "mcp_tool_call", connector_id: "computer-use", connector_name: "Computer Use",
    persist: ["session", "always"], progressToken: 0, riskLevel: "low", tool_name: "get_app_state",
    tool_params: { app: "com.example.CuaFixture" },
    tool_params_display: [{ display_name: "App", name: "app", value: "CUA Fixture" }],
  },
  message: 'Allow Computer Use to use "CUA Fixture"?', mode: "form",
  requestedSchema: { properties: {}, type: "object" }, futureField: { preserved: true },
};
const context = (signal = new AbortController().signal, callId = "call") => ({ sessionId: "form-session", callId, model: "fixture", parentCallId: "", signal });
const catalog = [{ name: "js", inputSchema: { type: "object" } }];
function provider({ requestParams = params, method = "elicitation/create", cancel = false, complete = false, disconnect = false } = {}) {
  const script = `
    let capabilities, call;
    const send = value => process.stdout.write(JSON.stringify({jsonrpc:'2.0',...value})+'\\n');
    require('readline').createInterface({input:process.stdin}).on('line', line => {
      const request = JSON.parse(line);
      if (request.method === 'initialize') {
        capabilities = request.params.capabilities;
        send({id:request.id,result:{protocolVersion:'2025-06-18',capabilities:{tools:{}}}});
      } else if (request.method === 'tools/list') send({id:request.id,result:{tools:${JSON.stringify(catalog)}}});
      else if (request.method === 'tools/call') {
        call = request.id;
        send({id:0,method:${JSON.stringify(method)},params:${JSON.stringify(requestParams)}});
        if (${disconnect}) setTimeout(() => process.exit(0), 30);
        if (${complete}) setTimeout(() => send({id:call,result:{content:[{type:"text",text:JSON.stringify({completed:true})}]}}), 30);
        if (${cancel}) setTimeout(() => send({method:'notifications/cancelled',params:{requestId:0}}), 30);
      } else if (request.id === 0) send({id:call,result:{content:[{type:'text',text:JSON.stringify({capabilities,response:request})}]}});
    });`;
  return { executable: process.execPath, args: ["-e", script] };
}
async function response(attachment, ctx = context()) {
  const result = await attachment.tool("js").handler({}, ctx);
  return JSON.parse(result.output[0].text);
}

for (const action of ["accept", "decline", "cancel"]) {
  test(`host ${action} preserves raw form parameters, context and complete response`, async t => {
    let seen;
    const result = { action, content: { confirmed: action === "accept" }, _meta: { persist: "session", custom: [1, 2] } };
    const attachment = await connectComputerTools({ ...provider(), elicitationHandler: (request, ctx) => { seen = { request, ctx }; return result; } });
    t.after(attachment.close);
    const actual = await response(attachment);
    assert.deepEqual(actual.capabilities, { elicitation: { form: {} } });
    assert.deepEqual(actual.response.result, result);
    assert.deepEqual(seen.request, params);
    assert.deepEqual({ ...seen.ctx, signal: undefined }, { sessionId: "form-session", callId: "call", model: "fixture", requestId: 0, signal: undefined });
    assert(seen.ctx.signal instanceof AbortSignal);
    await response(attachment, context(undefined, "next-call"));
    assert.equal(seen.ctx.callId, "next-call");
  });
}

test("MCP omitted mode defaults to form", async t => {
  const { mode, ...requestParams } = params;
  const attachment = await connectComputerTools({ ...provider({ requestParams }), elicitationHandler: received => {
    assert.deepEqual(received, requestParams);
    return { action: "accept" };
  } });
  t.after(attachment.close);
  assert.deepEqual((await response(attachment)).response.result, { action: "accept" });
});

test("no handler neither advertises capability nor accepts a provider request", async t => {
  const attachment = await connectComputerTools(provider());
  t.after(attachment.close);
  const actual = await response(attachment);
  assert.deepEqual(actual.capabilities, {});
  assert.equal(actual.response.error.code, -32601);
});

for (const [name, options, code] of [
  ["URL mode", { requestParams: { ...params, mode: "url" } }, -32602],
  ["malformed schema", { requestParams: { ...params, requestedSchema: [] } }, -32602],
  ["legacy method", { method: "openai/form" }, -32601],
]) {
  test(`rejects ${name} without invoking the form handler`, async t => {
    const attachment = await connectComputerTools({ ...provider(options), elicitationHandler: () => { assert.fail("unsupported request invoked host"); } });
    t.after(attachment.close);
    assert.equal((await response(attachment)).response.error.code, code);
  });
}

for (const trigger of ["timeout", "provider cancellation"]) {
  test(`${trigger} aborts host signal and returns cancel without late acceptance`, async t => {
    let signal, resolve;
    const attachment = await connectComputerTools({ ...provider({ cancel: trigger === "provider cancellation" }), elicitationTimeoutMs: trigger === "timeout" ? 30 : 5000,
      elicitationHandler: (_params, ctx) => { signal = ctx.signal; return new Promise(done => { resolve = done; }); } });
    t.after(attachment.close);
    const actual = await response(attachment);
    assert.deepEqual(actual.response.result, { action: "cancel" });
    assert.equal(signal.aborted, true);
    resolve({ action: "accept", _meta: { persist: "always" } });
    // A second request proves no late response polluted the transport.
    assert.deepEqual((await response(attachment)).response.result, { action: "cancel" });
  });
}

for (const trigger of ["close", "caller abort", "releaseSession"]) {
  test(`${trigger} aborts an in-flight form and rejects its tool call`, async t => {
    let started;
    const ready = new Promise(resolve => { started = resolve; });
    let signal;
    const attachment = await connectComputerTools({ ...provider(), elicitationHandler: (_params, ctx) => {
      signal = ctx.signal; started(); return new Promise(() => {});
    } });
    t.after(attachment.close);
    const caller = new AbortController();
    const rejected = assert.rejects(response(attachment, context(caller.signal)));
    await ready;
    if (trigger === "close") await attachment.close();
    else if (trigger === "caller abort") caller.abort(new Error("caller stopped"));
    else attachment.tool("js").releaseSession("form-session");
    await rejected;
    assert.equal(signal.aborted, true);
  });
}

for (const [name, handler, code] of [
  ["invalid action", () => ({ action: "yes" }), -32602],
  ["invalid metadata", () => ({ action: "accept", _meta: [] }), -32602],
  ["thrown host error", () => { throw new Error("private host detail"); }, -32603],
]) {
  test(`${name} cannot approve a request`, async t => {
    const attachment = await connectComputerTools({ ...provider(), elicitationHandler: handler });
    t.after(attachment.close);
    const actual = await response(attachment);
    assert.equal(actual.response.error.code, code);
    assert(!JSON.stringify(actual).includes("private host detail"));
  });
}

test("approval timeout and handler configuration are validated before spawning", () => {
  for (const elicitationTimeoutMs of [0, -1, Infinity, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => createComputerTools({ ...provider(), definitions: catalog, elicitationTimeoutMs }), /positive safe integer/);
  }
  assert.throws(() => createComputerTools({ ...provider(), definitions: catalog, elicitationHandler: true }), /function/);
});

for (const trigger of ["call completion", "provider disconnect"]) {
  test(`${trigger} dismisses pending UI and ignores late consent`, async t => {
    let signal, resolve;
    const attachment = await connectComputerTools({ ...provider({ complete: trigger === "call completion", disconnect: trigger === "provider disconnect" }),
      elicitationHandler: (_params, ctx) => { signal = ctx.signal; return new Promise(done => { resolve = done; }); } });
    t.after(attachment.close);
    if (trigger === "provider disconnect") await assert.rejects(response(attachment));
    else assert.deepEqual(await response(attachment), { completed: true });
    assert.equal(signal.aborted, true);
    resolve({ action: "accept", _meta: { persist: "always" } });
    if (trigger === "call completion") assert.deepEqual(await response(attachment), { completed: true });
  });
}

for (const mode of ["form", "url"]) {
  test(`Codex openai/elicitation/create alias uses the same ${mode} validation`, async t => {
    let seen;
    const result = { action: "accept", content: {}, _meta: { receipt: "explicit-host-choice" } };
    const requestParams = { ...params, mode };
    const attachment = await connectComputerTools({ ...provider({ method: "openai/elicitation/create", requestParams }),
      elicitationHandler: request => { seen = request; return result; } });
    t.after(attachment.close);
    const actual = await response(attachment);
    if (mode === "form") {
      assert.deepEqual(seen, requestParams);
      assert.deepEqual(actual.response.result, result);
    } else {
      assert.equal(seen, undefined);
      assert.equal(actual.response.error.code, -32602);
    }
  });
}
