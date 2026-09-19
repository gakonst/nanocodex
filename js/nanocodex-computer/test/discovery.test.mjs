import test from "node:test";
import assert from "node:assert/strict";
import { connectComputerTools } from "../index.mjs";
import { CUA_PARAMETERS, CUA_RESET_PARAMETERS } from "../contract.mjs";

const context = { sessionId: "discovery", callId: "call", parentCallId: "", model: "fixture", signal: new AbortController().signal };
function provider(mode = "ok") {
  const catalog = [
    { name: "js", description: "Provider initialization: await desktop.connect()", inputSchema: CUA_PARAMETERS },
    { name: "js_reset", description: "Provider reset description", inputSchema: CUA_RESET_PARAMETERS },
  ];
  const script = `
    const catalog = ${JSON.stringify(catalog)};
    const mode = ${JSON.stringify(mode)};
    if (process.argv.length !== 1) process.exit(9);
    let initialized = false;
    require('readline').createInterface({ input: process.stdin }).on('line', line => {
      const request = JSON.parse(line);
      if (!request.id) { initialized = true; return; }
      let result;
      if (request.method === 'initialize') result = { protocolVersion: '2025-03-26', capabilities: {tools:{}} };
      else if (request.method === 'tools/list') {
        if (!initialized) process.exit(10);
        if (mode === 'schema') catalog[0].inputSchema = { type: 'object', properties: { invented: {type:'string'} } };
        result = request.params.cursor ? { tools: [catalog[1]] } : { tools: [catalog[0]], nextCursor: 'second' };
        if (mode === 'cursor') result.nextCursor = 'second';
        if (mode === 'duplicate') result.tools = [catalog[0],catalog[0],catalog[1]];
      } else if (request.method === 'tools/call') result = { content: [{ type: 'text', text: JSON.stringify(request.params) }], _meta: { provider: 'fixture' } };
      process.stdout.write(JSON.stringify({jsonrpc:'2.0', id:request.id, result})+'\\n');
    });`;
  return { options: { executable: process.execPath, args: ["-e", script], transport: "mcp" }, catalog };
}

test("discovers paginated external MCP declarations and dispatches exact names and arguments", async t => {
  const { options, catalog } = provider();
  const attachment = await connectComputerTools(options);
  t.after(attachment.close);
  assert.deepEqual(attachment.tools.map(({ name, description, parameters }) => ({ name, description, parameters })),
    catalog.map(({ name, description, inputSchema }) => ({ name: `mcp__cua_repl__${name}`, description, parameters: inputSchema })));
  const input = { code: "await desktop.connect()", title: "Connect provider", timeout_ms: 1000 };
  const result = await attachment.tools[0].handler(input, context);
  const call = JSON.parse(result.output[0].text);
  assert.equal(call.name, "js");
  assert.deepEqual(call.arguments, input);
  assert.deepEqual(call._meta["x-codex-turn-metadata"], { thread_id: "discovery", call_id: "call", model: "fixture" });
  assert.deepEqual(result.metadata, { provider: "fixture" });
  assert.equal(JSON.parse((await attachment.tools[1].handler({}, context)).output[0].text).name, "js_reset");
});

for (const [mode, message] of [["schema", /schema is unsupported/], ["cursor", /repeated tools\/list cursor/], ["duplicate", /exactly one js/]]) {
  test(`rejects provider ${mode} before publishing tools`, async () => {
    await assert.rejects(connectComputerTools(provider(mode).options), message);
  });
}
