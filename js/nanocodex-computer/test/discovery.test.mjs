import test from "node:test";
import assert from "node:assert/strict";
import { connectComputerTools, createComputerTools } from "../index.mjs";
import { hostedAppToolCatalog } from "nanocodex-tools/hosted-catalog";

const context = { sessionId: "discovery", callId: "call", parentCallId: "", model: "fixture", signal: new AbortController().signal };
function provider(mode = "ok") {
  const catalog = [
    { name: "js", description: "Provider initialization: await desktop.connect()", inputSchema: { type: "object", properties: { source: { type: "string" } }, required: ["source"] }, annotations: { readOnlyHint: false } },
    { name: "js_add_node_module_dir", description: "Add a provider module directory", inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] }, outputSchema: { type: "object" }, _meta: { custom: { preserved: true } } },
    { name: "js_reset", inputSchema: { type: "object", properties: { reason: { type: "string" } } } },
    { name: "turn_ended", description: "Trusted lifecycle hook", inputSchema: { type: "object", properties: { turn_id: { type: "string" } } }, _meta: { ui: { visibility: [] } } },
    { name: "future_tool", inputSchema: { type: "object", additionalProperties: true }, annotations: { idempotentHint: true } },
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
        result = request.params.cursor ? { tools: catalog.slice(1) } : { tools: [catalog[0]], nextCursor: 'second' };
        if (mode === 'cursor') result.nextCursor = 'second';
        if (mode === 'duplicate') result.tools = [catalog[0],catalog[0]];
        if (mode === 'schema') result.tools = [{...catalog[0], inputSchema: []}];
      } else if (request.method === 'tools/call') result = { content: [{ type: 'text', text: JSON.stringify(request.params) }], _meta: { provider: 'fixture' } };
      process.stdout.write(JSON.stringify({jsonrpc:'2.0', id:request.id, result})+'\\n');
    });`;
  return { options: { executable: process.execPath, args: ["-e", script], transport: "mcp" }, catalog };
}

test("discovers all paginated provider contracts without rewriting schemas or optional metadata", async t => {
  const { options, catalog } = provider();
  const attachment = await connectComputerTools(options);
  t.after(attachment.close);
  assert.deepEqual(attachment.definitions, catalog);
  assert.deepEqual(attachment.tools.map(tool => tool.providerDefinition), catalog.filter(tool => tool.name !== "turn_ended"));
  assert.deepEqual(attachment.tools.map(tool => tool.name), ["js", "js_add_node_module_dir", "js_reset", "future_tool"].map(name => `mcp__cua_repl__${name}`));
  assert.deepEqual(attachment.tool("js_add_node_module_dir").outputSchema, { type: "object" });
  for (const [name, input] of [["js", { source: "provider syntax", timeout_ms: "provider-owned" }], ["js_add_node_module_dir", { path: "/fixture/node_modules" }], ["js_reset", { reason: "provider reset" }], ["future_tool", { nested: { value: 3 } }], ["turn_ended", { turn_id: "turn" }]]) {
    const result = await attachment.tool(name).handler(input, context);
    const call = JSON.parse(result.output[0].text);
    assert.equal(call.name, name);
    assert.deepEqual(call.arguments, input);
    assert.deepEqual(call._meta["x-codex-turn-metadata"], { thread_id: "discovery", call_id: "call", model: "fixture" });
    assert.deepEqual(result.metadata, { provider: "fixture" });
  }
});

for (const [mode, message] of [["schema", /JSON schema object/], ["cursor", /repeated tools\/list cursor/], ["duplicate", /unique/]]) {
  test(`rejects malformed provider ${mode} before publishing tools`, async () => {
    await assert.rejects(connectComputerTools(provider(mode).options), message);
  });
}


test("matches Codex visibility semantics before emitting the account hosted catalog", async t => {
  // codex-mcp/src/connection_manager/tool_catalog.rs::tool_is_model_visible
  const cases = [
    [undefined, true], [{}, true], [{ ui: {} }, true],
    [{ ui: { visibility: "model" } }, true],
    [{ ui: { visibility: null } }, true],
    [{ ui: { visibility: [] } }, false],
    [{ ui: { visibility: ["app"] } }, false],
    [{ ui: { visibility: ["model"] } }, true],
    [{ ui: { visibility: ["app", "model"] } }, true],
    [{ ui: { visibility: [null, 3, { model: true }] } }, false],
  ];
  const definitions = cases.map(([meta], index) => ({
    name: `visibility_${index}`, inputSchema: { type: "object" },
    ...(meta === undefined ? {} : { _meta: meta }),
  }));
  const attachment = createComputerTools({ executable: process.execPath, transport: "mcp", definitions });
  t.after(attachment.close);
  const visible = cases.flatMap(([, visible], index) => visible ? [`mcp__cua_repl__visibility_${index}`] : []);
  assert.deepEqual(attachment.definitions, definitions);
  assert.deepEqual(attachment.tools.map(tool => tool.name), visible);
  assert.deepEqual(hostedAppToolCatalog(attachment.tools).map(entry => entry.definition.name).sort(), [...visible].sort());
  for (const definition of definitions) assert(attachment.tool(definition.name), "trusted lifecycle lookup retains hidden catalog entries");
});


test("explicit discovery catalogs remain pinned including hidden tool metadata", async t => {
  const { options, catalog } = provider();
  const changed = structuredClone(catalog);
  changed.find(tool => tool.name === "turn_ended")._meta.ui.visibility = ["app"];
  const attachment = createComputerTools({ ...options, definitions: changed });
  t.after(attachment.close);
  await assert.rejects(attachment.tool("js").handler({ source: "unchanged visible schema" }, context), /catalog changed/);
});
