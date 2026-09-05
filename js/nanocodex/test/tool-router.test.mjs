import assert from "node:assert/strict";
import test from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import {
  logicalContractFingerprint,
  preDispatchUnavailable,
  providerSource,
  ToolRouter,
  toolMapSource,
} from "../runtime/tool-router.mjs";
import { createTools } from "../tools/Tools.mjs";
import { createWorkspace } from "../runtime/workspace.mjs";
import { resolveTools } from "../runtime/tool-configuration.mjs";

const contract = (name, extra = {}) => ({
  type: "function",
  name,
  description: `Call ${name}.`,
  strict: true,
  parameters: { type: "object", properties: {}, additionalProperties: false },
  output_schema: { type: "object", additionalProperties: true },
  ...extra,
});

test("createTools rejects ambiguous and orphaned configuration", async () => {
  await assert.rejects(createTools({ customTools: {} }), /unsupported createTools option/);
  await assert.rejects(createTools({ workspaceOptions: {} }), /requires workspace/);
  await assert.rejects(createTools({ mcp: false, mcpOptions: {} }), /requires mcp/);
  await assert.rejects(createTools({ tools: null }), /tools must be/);
});

test("createTools transfers caller tool ownership only after successful construction", async () => {
  let disposals = 0;
  const workspace = createWorkspace({ backend: {
    async list() { return []; },
    async readFile() { return new Uint8Array(); },
    async writeFile() {},
    async remove() {},
    async mkdir() {},
  } });
  await assert.rejects(
    createTools({
      tools: {
        read_file: {
          handler() {},
          dispose() { disposals++; },
        },
      },
      workspace,
    }),
    /duplicate tool name/,
  );
  assert.equal(disposals, 0);
});

test("Tools close joins one exhaustive synchronous and asynchronous cleanup", async () => {
  const events = [];
  const tools = await createTools({ tools: {
    first: {
      handler() {},
      dispose() { events.push("first"); throw new Error("first cleanup failed"); },
    },
    second: {
      handler() {},
      async dispose() { events.push("second"); throw new Error("second cleanup failed"); },
    },
    third: {
      handler() {},
      dispose() { events.push("third"); },
    },
  } });
  const first = tools.close();
  const second = tools.close();
  assert.equal(first, second);
  await assert.rejects(first, (error) => {
    assert(error instanceof AggregateError);
    assert.deepEqual(error.errors.map(({ message }) => message), [
      "first cleanup failed",
      "second cleanup failed",
    ]);
    return true;
  });
  assert.deepEqual(events, ["first", "second", "third"]);
  assert.equal(tools.close(), first);
});

test("a cleanup callback may reenter its owning Tools close", async () => {
  let tools;
  tools = await createTools({ tools: { reentrant: {
    handler() {},
    dispose: () => tools.close(),
  } } });
  const closing = tools.close();
  assert.equal(tools.close(), closing);
  await closing;
});

test("a closeable provider owns its tools' cleanup exactly once", async () => {
  const events = [];
  const tool = {
    name: "echo",
    handler: () => "ok",
    parallelSafe: true,
    dispose: () => events.push("tool.dispose"),
  };
  const provider = {
    definitions: () => [contract("echo")],
    resolve: () => tool,
    close() {
      events.push("provider.close");
      tool.dispose();
    },
  };
  const router = new ToolRouter([providerSource("provider", provider)]);
  const first = router.reset();
  assert.equal(router.reset(), first);
  await first;
  assert.deepEqual(events, ["provider.close", "tool.dispose"]);
});

test("a forged structural Tools object is rejected instead of becoming a tool map", () => {
  const standalone = { defaultSubagents: false };
  assert.deepEqual(resolveTools(undefined, standalone), { tools: {}, subagents: undefined });
  assert.throws(() => resolveTools(null, standalone), /tools must be/);
  assert.throws(
    () => resolveTools({ attach() {}, close() {} }, standalone),
    /was not created by createTools/,
  );
});

function source(id, entries, options = {}) {
  const tools = new Map(entries.map(({
    definition,
    handler = () => id,
    parallelSafe = true,
    provider,
    remoteName,
    summary,
    timeoutMs,
  }) => [
    definition.name,
    { name: definition.name, handler, parallelSafe, provider, remoteName, summary, timeoutMs },
  ]));
  return {
    id,
    kind: options.kind ?? "union",
    mode: options.mode ?? "union",
    definitions: () => entries.map(({ definition }) => definition),
    resolve: (name) => tools.get(name),
    ...(options.search ? { search: options.search } : {}),
  };
}

test("source permutations produce byte-identical deterministic snapshots", () => {
  const a = source("a", [{ definition: contract("alpha") }]);
  const z = source("z", [{ definition: contract("zeta") }]);
  const left = new ToolRouter([z, a]);
  const right = new ToolRouter([a, z]);
  assert.equal(JSON.stringify(left.definitions()), JSON.stringify(right.definitions()));
  assert.deepEqual(left.definitions().map(({ name }) => name), ["alpha", "zeta"]);
});

test("duplicate and normalized collisions reject and addSource rolls back", () => {
  const router = new ToolRouter([source("first", [{ definition: contract("a.b") }])]);
  assert.throws(
    () => router.addSource(source("second", [{ definition: contract("a_b") }])),
    /normalized tool name collision/,
  );
  assert.deepEqual(router.definitions().map(({ name }) => name), ["a.b"]);
  assert.throws(
    () => router.addSource(source("third", [{ definition: contract("a.b") }])),
    /duplicate tool name/,
  );
  assert.deepEqual(router.definitions().map(({ name }) => name), ["a.b"]);
});

test("attached overlay requires callable parity but keeps placement metadata independent", async () => {
  const entry = {
    definition: contract("echo"),
    parallelSafe: true,
    provider: "javascript",
    remoteName: "echo",
    summary: "Echo locally.",
    timeoutMs: 42_000,
  };
  const cloud = source("cloud", [{ ...entry, handler: () => "cloud" }], { kind: "cloud" });
  for (const changed of [
    { ...entry, definition: contract("echo", { strict: false }) },
    { ...entry, definition: contract("echo", { output_schema: { type: "string" } }) },
  ]) {
    const router = new ToolRouter([cloud]);
    await assert.rejects(
      router.attachSource(source("attached", [changed], { kind: "attached" })),
      /catalog parity mismatch/,
    );
  }
  const router = new ToolRouter([cloud]);
  await router.attachSource(source("attached", [{
    ...entry,
    definition: contract("echo", { description: "Echo through the attached host." }),
    provider: "device",
    remoteName: "remote_echo",
    parallelSafe: false,
    summary: "Different placement summary.",
    timeoutMs: 42_001,
    handler: () => "attached",
  }], { kind: "attached" }));
  assert.equal(router.resolve("echo").parallelSafe, false);
  assert.equal(router.modelDefinitions().find(({ name }) => name === "echo").description, "Call echo.");
  assert.equal(
    logicalContractFingerprint(contract("echo")),
    logicalContractFingerprint(contract("echo", { defer_loading: true })),
  );
});

test("pre-publication validation consumes a complete hosted catalog candidate", () => {
  const cloud = source("cloud", [{
    definition: contract("echo"),
    parallelSafe: false,
    provider: "javascript",
    remoteName: "echo",
    timeoutMs: 120_000,
  }], { kind: "cloud" });
  const router = new ToolRouter([cloud]);
  assert.equal(router.validateSource({
    id: "attached",
    kind: "attached",
    mode: "attached-over-cloud",
    definitions: () => [{
      provider: "javascript",
      remote_name: "echo",
      definition: contract("echo", { defer_loading: true }),
      parallel_safe: false,
      timeout_ms: 120_000,
    }],
    resolve: (name) => ({ name, handler() {}, parallelSafe: true }),
  }), true);
  assert.equal(router.validateSource({
    id: "attached",
    kind: "attached",
    mode: "attached-over-cloud",
    definitions: () => [{
      provider: "javascript",
      remote_name: "echo",
      definition: contract("echo", { defer_loading: true }),
      parallel_safe: true,
      timeout_ms: 120_000,
    }],
    resolve: (name) => ({ name, handler() {}, parallelSafe: false }),
  }), true);
});

test("overlay keeps cloud fallback but only uses the typed pre-dispatch sentinel", async () => {
  let attachedValue = "attached";
  let cloudCalls = 0;
  const router = new ToolRouter([
    source("cloud", [{ definition: contract("echo"), handler: () => { cloudCalls++; return "cloud"; } }], { kind: "cloud" }),
  ]);
  await router.attachSource(source("attached", [{
    definition: contract("echo", { defer_loading: true }),
    handler: () => attachedValue,
  }], { kind: "attached" }));
  const context = { signal: new AbortController().signal };
  assert.equal(await router.execute("echo", {}, context), "attached");
  assert.equal(cloudCalls, 0);
  attachedValue = { [preDispatchUnavailable]: true };
  assert.equal(await router.execute("echo", {}, context), "cloud");
  assert.equal(cloudCalls, 1);
  attachedValue = { success: false, output: "ordinary failure" };
  assert.deepEqual(await router.execute("echo", {}, context), attachedValue);
  assert.equal(cloudCalls, 1);
});

test("overlay schedules for the least parallel-safe possible placement", async () => {
  let active = 0;
  let maxActive = 0;
  const router = new ToolRouter([
    source("cloud", [{
      definition: contract("echo"),
      parallelSafe: false,
      async handler() {
        active++;
        maxActive = Math.max(maxActive, active);
        await new Promise((resolve) => setImmediate(resolve));
        active--;
        return "cloud";
      },
    }], { kind: "cloud" }),
  ]);
  await router.attachSource(source("attached", [{
    definition: contract("echo", { defer_loading: true }),
    parallelSafe: true,
    handler: () => ({ [preDispatchUnavailable]: true }),
  }], { kind: "attached" }));
  assert.equal(router.resolve("echo").parallelSafe, false);
  await Promise.all([
    router.execute("echo", {}, { signal: new AbortController().signal }),
    router.execute("echo", {}, { signal: new AbortController().signal }),
  ]);
  assert.equal(maxActive, 1);
});

test("admitted snapshots pin attach/detach and definitions", async () => {
  const router = new ToolRouter([source("cloud", [{ definition: contract("echo"), handler: () => "cloud" }], { kind: "cloud" })]);
  const admission = await router.admit();
  const attaching = router.attachSource(source("attached", [{ definition: contract("echo"), handler: () => "attached" }], { kind: "attached" }));
  let settled = false;
  void attaching.then(() => { settled = true; });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(settled, false);
  assert.equal(await admission.invoke("echo", {}, { signal: new AbortController().signal }), "cloud");
  assert(Object.isFrozen(admission.definitions));
  admission.release();
  await attaching;
  assert.equal(await router.execute("echo", {}, { signal: new AbortController().signal }), "attached");
  const next = await router.admit();
  const detaching = router.detachSource("attached");
  assert.equal(await next.invoke("echo", {}, { signal: new AbortController().signal }), "attached");
  next.release();
  await detaching;
  assert.equal(await router.execute("echo", {}, { signal: new AbortController().signal }), "cloud");
});

test("one router-owned tool_search merges MCP and attached discovery", async () => {
  const mcpDefinition = contract("mcp__docs__lookup", { defer_loading: true });
  const attachedDefinition = contract("device_lookup");
  const mcpSearch = () => ({
    [Symbol.for("nanocodex.toolResult")]: true,
    output: { tools: [
      { name: "mcp__docs__lookup", server: "docs" },
      { name: "mcp__docs__not_admitted", server: "docs" },
    ] },
    structuredResult: [{
      type: "namespace",
      name: "mcp__docs__",
      description: "Documentation tools.",
      tools: [
        contract("lookup", { output_schema: undefined }),
        contract("not_admitted", { output_schema: undefined }),
      ],
    }],
  });
  const router = new ToolRouter([
    source("mcp", [
      { definition: { type: "tool_search", execution: "client", description: "old", parameters: {} } },
      { definition: mcpDefinition },
    ], { kind: "mcp", search: mcpSearch }),
    source("device", [{ definition: attachedDefinition }], { kind: "attached", mode: "attached-over-cloud" }),
  ]);
  assert.equal(router.definitions().filter(({ type }) => type === "tool_search").length, 1);
  const found = await router.execute("tool_search", { query: "lookup" }, { signal: new AbortController().signal });
  assert.deepEqual(found.structuredResult[0].tools.map(({ name }) => name).sort(), [
    "lookup",
  ]);
  assert.deepEqual(found.structuredResult.map(({ type, name }) => ({ type, name })), [
    { type: "namespace", name: "mcp__docs__" },
    { type: "function", name: "device_lookup" },
  ]);
  assert.equal("output_schema" in found.structuredResult[1], false);
  assert.deepEqual(found.output.tools.map(({ name }) => name).sort(), ["device_lookup", "mcp__docs__lookup"]);
});

test("tool_search returns only provider-loadable definitions", async () => {
  const router = new ToolRouter([
    source("device", [{ definition: contract("accountInfo") }], {
      kind: "attached",
      mode: "attached-over-cloud",
    }),
  ]);
  const context = { signal: new AbortController().signal };
  const found = await router.execute("tool_search", { query: "account info" }, context);
  assert.deepEqual(found.structuredResult, [{
    type: "function",
    name: "accountInfo",
    description: "Call accountInfo.",
    strict: true,
    defer_loading: true,
    parameters: { type: "object", properties: {}, additionalProperties: false },
  }]);
  const empty = await router.execute("tool_search", { query: "unrelated" }, context);
  assert.deepEqual(empty.output.tools, []);
  assert.deepEqual(empty.structuredResult, []);
});

test("search providers own namespaced ranking and duplicate loading", async () => {
  const exact = contract("mcp__fixture__exact", { defer_loading: true });
  const ranked = contract("mcp__fixture__ranked", { defer_loading: true });
  const namespace = {
    type: "namespace",
    name: "mcp__fixture__",
    description: "Fixture tools.",
    tools: [
      contract("ranked", { output_schema: undefined }),
      contract("ranked", { output_schema: undefined }),
    ],
  };
  const router = new ToolRouter([source("mcp", [
    { definition: { type: "tool_search", execution: "client", description: "Search MCP.", parameters: {} } },
    { definition: exact },
    { definition: ranked },
  ], {
    kind: "mcp",
    search: () => ({
      [Symbol.for("nanocodex.toolResult")]: true,
      output: { tools: [{ name: "mcp__fixture__ranked" }] },
      structuredResult: [namespace, namespace],
    }),
  })]);
  const found = await router.execute("tool_search", { query: "exact", limit: 1 }, {
    signal: new AbortController().signal,
  });
  assert.deepEqual(found.output.tools.map(({ name }) => name), ["mcp__fixture__ranked"]);
  assert.deepEqual(found.structuredResult.map(({ type, name, tools }) => ({
    type,
    name,
    tools: tools.map((tool) => tool.name),
  })), [{
    type: "namespace",
    name: "mcp__fixture__",
    tools: ["ranked"],
  }]);
});

test("createCodeRuntime adopts a branded createTools router and carries the pinned model context", async () => {
  let disposals = 0;
  const tools = await createTools({ tools: { echo: {
    handler: ({ value }, context) => `${value}:${context.model}`,
    dispose: () => { disposals++; },
  } } });
  const runtime = createCodeRuntime(tools);
  assert.equal(
    JSON.parse(await runtime.executeTool("echo", '{"value":"adopted"}', "session:1", "call:1", "gpt-5.6-luna")).output,
    "adopted:gpt-5.6-luna",
  );
  await runtime.reset();
  assert.equal(disposals, 0);
  assert.equal(
    JSON.parse(await runtime.executeTool("echo", '{"value":"still-owned"}', "session:1", "call:2", "gpt-5.6-luna")).output,
    "still-owned:gpt-5.6-luna",
  );
  await tools.close();
  assert.equal(disposals, 1);
});

test("an empty attached source reserves byte-stable model tool_search before later catalogs", () => {
  let entries = [];
  const attached = source("attached", entries, { kind: "attached", mode: "attached-over-cloud" });
  attached.definitions = () => entries.map(({ definition }) => definition);
  attached.resolve = (name) => entries.find(({ definition }) => definition.name === name)?.tool;
  const router = new ToolRouter([attached]);
  const before = JSON.stringify(router.modelDefinitions());
  assert.equal(JSON.parse(before)[0].type, "tool_search");
  entries = [{ definition: contract("later"), tool: { name: "later", parallelSafe: true, handler: () => "ok" } }];
  assert.equal(JSON.stringify(router.modelDefinitions()), before);
  assert.deepEqual(router.definitions().map((definition) => definition.type === "tool_search" ? "tool_search" : definition.name), [
    "tool_search", "later",
  ]);
});

test("direct and Code Mode calls share the exclusive scheduler", async () => {
  let release;
  let directStarted;
  const started = new Promise((resolve) => { directStarted = resolve; });
  const runtime = createCodeRuntime({
    exclusive: { async handler({ direct }) { if (direct) { directStarted(); await new Promise((resolve) => { release = resolve; }); } return direct; } },
  });
  const direct = runtime.executeTool("exclusive", JSON.stringify({ direct: true }));
  await started;
  const code = runtime.executeCode("text(await tools.exclusive({ direct: false }));");
  let codeSettled = false;
  void code.then(() => { codeSettled = true; });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(codeSettled, false);
  release();
  await direct;
  assert.equal(JSON.parse(await code).success, true);
});

test("createTools accepts the portable WorkspaceBackend shape", async () => {
  const files = new Map();
  const workspace = createWorkspace({ backend: {
    async list() { return [...files].map(([path, bytes]) => ({ kind: "file", path, size: bytes.length })); },
    async readFile(path) { return files.get(path); },
    async writeFile(path, bytes) { files.set(path, bytes); },
    async remove(path) { files.delete(path); },
    async mkdir() {},
  } });
  const tools = await createTools({ workspace });
  const runtime = createCodeRuntime(tools);
  assert.deepEqual(JSON.parse(runtime.toolDefinitions()).map(({ name }) => name).sort(), [
    "delete_file", "list_files", "make_directory", "read_file", "write_file",
  ]);
  const written = JSON.parse(await runtime.executeTool(
    "write_file",
    JSON.stringify({ path: "rn.txt", content: "portable" }),
  ));
  assert.equal(written.success, true);
  assert.equal(new TextDecoder().decode(files.get("rn.txt")), "portable");
  await tools.close();
});
