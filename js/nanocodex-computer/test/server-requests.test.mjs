import test from "node:test";
import assert from "node:assert/strict";
import { connectComputerTools } from "../index.mjs";

const catalog = [{ name: "js", inputSchema: { type: "object" } }];
const context = { sessionId: "protocol-session", callId: "call", model: "fixture", parentCallId: "", signal: new AbortController().signal };
function provider(method, params) {
  const script = `
    let capabilities, pending;
    const responses = [];
    const send = value => process.stdout.write(JSON.stringify({jsonrpc:'2.0',...value})+'\\n');
    require('readline').createInterface({input:process.stdin}).on('line', line => {
      const request = JSON.parse(line);
      if (!request.method) {
        if (!pending || request.id !== pending.providerId) process.exit(9);
        responses.push(request);
        const {id, result} = pending;
        pending = undefined;
        send({id,result:result ?? {content:[{type:'text',text:JSON.stringify({capabilities,responses})}]}});
      } else if (request.method !== 'notifications/initialized') {
        let result, providerId;
        if (request.method === 'initialize') {
          capabilities = request.params.capabilities;
          result = {protocolVersion:'2025-06-18',capabilities:{tools:{}}};
          providerId = 0;
        } else if (request.method === 'tools/list') {
          result = {tools:${JSON.stringify(catalog)}};
          providerId = 'provider-request';
        } else if (request.method === 'tools/call') {
          // JSON-RPC request IDs are independent in each direction.
          providerId = request.id;
        } else process.exit(10);
        pending = {id:request.id,providerId,result};
        send({method:'notifications/cancelled',params:{requestId:providerId}});
        send({method:'provider/notification',params:{future:true}});
        send({id:providerId,method:${JSON.stringify(method)},params:${JSON.stringify(params)}});
      }
    });`;
  return { executable: process.execPath, args: ["-e", script] };
}

for (const [method, params] of [
  ["elicitation/create", { mode: "form", message: "Provider request", requestedSchema: { type: "object" } }],
  ["openai/elicitation/create", { mode: "url", url: "https://example.invalid/approval" }],
  ["elicitation/create", { requestedSchema: [] }],
  ["provider/future-request", null],
]) {
  test(`unsupported ${method} ${JSON.stringify(params)} returns protocol errors throughout the session`, { timeout: 10_000 }, async t => {
    const attachment = await connectComputerTools(provider(method, params));
    t.after(attachment.close);
    assert.deepEqual(attachment.definitions, catalog);
    for (let call = 0; call < 2; call++) {
      const result = await attachment.tool("js").handler({}, { ...context, callId: `call-${call}` });
      const actual = JSON.parse(result.output[0].text);
      assert.deepEqual(actual.capabilities, {});
      assert.deepEqual(actual.responses, [0, "provider-request", 3, ...(call ? [4] : [])].map(id => ({
        jsonrpc: "2.0", id, error: { code: -32601, message: "Method not found" },
      })));
    }
  });
}
