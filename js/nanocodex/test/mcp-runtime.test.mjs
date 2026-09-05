import assert from "node:assert/strict";
import { test } from "node:test";
import { McpError } from "@modelcontextprotocol/sdk/types.js";
import { Challenge, Credential, Mcp, Method } from "mppx";
import { Methods } from "mppx/tempo";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import { createMcpRuntime } from "../runtime/mcp-runtime.mjs";
import {
  createTempoProvider,
  createTempoProviderFromAccounts,
  DEFAULT_MERCATOR_MCP_URL,
  pinnedScopedAccountParameters,
  resolveMcpServers,
} from "../runtime/tempo-provider.mjs";

test("Mercator is a paid default only for explicit Tempo provider mode", () => {
  const session = { ws: async () => ({}) };
  const payment = { methods: [{}] };
  assert.throws(
    () => createTempoProvider({ session, payment: { methods: [] } }),
    /at least one MPPx method/,
  );
  const provider = createTempoProvider({ session, payment });

  assert.equal(resolveMcpServers(session, undefined), undefined);
  assert.equal(resolveMcpServers(undefined, undefined), undefined);
  assert.equal(resolveMcpServers(provider, false), undefined);
  assert.equal(provider.session, session);

  const defaults = resolveMcpServers(provider, undefined);
  assert.equal(defaults.mercator.url, DEFAULT_MERCATOR_MCP_URL);
  assert.equal(defaults.mercator.payment, payment);

  const custom = { client: { listTools() {}, callTool() {} } };
  assert.equal(resolveMcpServers(provider, { mercator: custom }).mercator, custom);
});

test("any Accounts SDK provider can own both Tempo payment paths", async () => {
  const accessKey = "0x0000000000000000000000000000000000000001";
  const calls = [];
  const walletParameters = {
    getClient() { return {}; },
    async resolveAccount() { return undefined; },
  };
  const wallet = {
    getMppxParameters(options) {
      calls.push(options);
      return walletParameters;
    },
  };

  const provider = await createTempoProviderFromAccounts({
    wallet,
    accessKey,
    policy: { maxDeposit: "0.05" },
    session: { bootstrap: true },
    payment: { maxAmount: 250_000n },
  });

  assert.deepEqual(calls, [{ accessKey }]);
  assert.equal(provider.kind, "tempo");
  assert.equal(typeof provider.ws, "function");
  assert.equal(typeof provider.fetch, "function");
  const mercator = resolveMcpServers(provider, undefined).mercator;
  assert.equal(mercator.url, DEFAULT_MERCATOR_MCP_URL);
  assert.equal(mercator.payment.methods.length, 1);
  assert.equal(mercator.payment.methods[0].length, 2);
  assert.equal(mercator.fetch, undefined);
  assert.equal(await mercator.payment.onPaymentRequired({ request: { amount: "250000" } }), true);
  await assert.rejects(
    mercator.payment.onPaymentRequired({ request: { amount: "250001" } }),
    /exceeds the per-request limit 250000/,
  );

  await assert.rejects(
    createTempoProviderFromAccounts({ wallet: {} }),
    /getMppxParameters/,
  );
  await assert.rejects(
    createTempoProviderFromAccounts({
      wallet: { getMppxParameters: () => ({}) },
    }),
    /invalid MPPx parameters/,
  );
});

test("pinned MPP clients read as the root and sign mutations with the access key", async () => {
  const root = "0x0000000000000000000000000000000000000002";
  const accessKey = "0x0000000000000000000000000000000000000003";
  const pinnedAccount = { address: root, accessKeyAddress: accessKey, type: "local" };
  let created;
  const sourceClient = {
    chain: {
      id: 4217,
      rpcUrls: { default: { http: ["https://rpc.tempo.example"] } },
    },
  };
  const parameters = {
    getClient() { return sourceClient; },
    async resolveAccount() { throw new Error("generic resolver must not be used"); },
  };
  const wallet = {
    store: {
      accessKeys: {
        async get(query) {
          assert.deepEqual(query, { account: root, accessKey, chainId: 4217 });
          return pinnedAccount;
        },
      },
      getState() {
        return { accounts: [{ address: root }], activeAccount: 0, chainId: 4217 };
      },
    },
  };

  const scoped = await pinnedScopedAccountParameters(wallet, parameters, accessKey, {
    account: root,
    chainId: 4217,
    createClient(options) {
      created = options;
      return options;
    },
    http(url) { return { url }; },
  });

  assert.deepEqual(scoped.getClient().account, { address: root, type: "json-rpc" });
  assert.equal(created.transport.url, "https://rpc.tempo.example");
  assert.equal(await scoped.resolveAccount({
    account: { address: root },
    chainId: 4217,
    operation: { kind: "authorizePaymentChannel", authority: accessKey },
  }), pinnedAccount);
});

test("remote MCP stays deferred behind tool_search and executes through Code Mode", async () => {
  const calls = [];
  const client = {
    async listTools(params) {
      if (!params?.cursor) {
        return {
          tools: [{
            name: "search_endpoints",
            description: "Find curated external services.",
            inputSchema: {
              type: "object",
              properties: { query: { type: "string" } },
              required: ["query"],
            },
          }],
          nextCursor: "paid",
        };
      }
      return {
        tools: [{
          name: "call",
          description: "Call a paid curated service.",
          inputSchema: {
            type: "object",
            properties: { service_id: { type: "string" } },
            required: ["service_id"],
          },
        }],
      };
    },
    async callTool(input) {
      calls.push(input);
      return { content: [{ type: "text", text: `called ${input.name}` }] };
    },
  };
  const mcp = await createMcpRuntime({
    mercator: {
      client,
      description: "Curated paid services through Mercator.",
    },
  });
  await mcp.settled();
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);

  const definitions = JSON.parse(runtime.toolDefinitions());
  assert.equal(definitions[0].type, "tool_search");
  assert.deepEqual(
    definitions.slice(1).map((definition) => [definition.name, definition.defer_loading]),
    [
      ["mcp__mercator__call", true],
      ["mcp__mercator__search_endpoints", true],
    ],
  );

  const searched = JSON.parse(await runtime.executeTool(
    "tool_search",
    JSON.stringify({ query: "paid service" }),
  ));
  assert.equal(searched.success, true);
  assert.equal(searched.structured_result[0].name, "mcp__mercator__");
  assert.equal(searched.structured_result[0].tools[0].defer_loading, true);
  assert.equal(JSON.parse(searched.output).tools[0].supports_parallel_tool_calls, false);

  const execution = JSON.parse(await runtime.executeCode(
    `const searchDefinition = ALL_TOOLS.find((tool) => tool.name === "tool_search");
    if (searchDefinition?.type !== "function") {
      throw new Error("Code Mode requires function-shaped tool_search metadata");
    }
    const found = await tools.tool_search({ query: "paid curated service" });
    const selected = found.tools.find((tool) => tool.name === "mcp__mercator__call");
    if (!selected) throw new Error("tool_search did not expose the MCP tool");
    const result = await tools[selected.name]({ service_id: "exa" });
    text(result);`,
    "session-1",
    "exec-1",
  ));
  assert.equal(execution.success, true);
  assert.deepEqual(calls, [{ name: "call", arguments: { service_id: "exa" } }]);
  assert.deepEqual(
    execution.nested_calls.map((call) => call.name),
    ["tool_search", "mcp__mercator__call"],
  );
  assert.equal(execution.nested_calls[0].structured_result[0].name, "mcp__mercator__");
  assert.match(JSON.stringify(execution.output), /called call/);
});

test("MCP server availability dynamically gates metadata, discovery, resolution, and calls", async () => {
  let available = false;
  let listCalls = 0;
  const calls = [];
  const mcp = await createMcpRuntime({
    account: {
      description: "Connected account tools.",
      isAvailable: () => available,
      client: {
        async listTools() {
          listCalls += 1;
          return {
            tools: [{
              name: "lookup",
              description: "Look up account records.",
              inputSchema: { type: "object" },
            }],
          };
        },
        async callTool(input) {
          calls.push(input);
          return { content: [{ type: "text", text: "found" }] };
        },
      },
    },
  });
  await mcp.settled();
  assert.equal(listCalls, 0);
  const name = "mcp__account__lookup";

  assert.deepEqual(
    mcp.definitions().map((definition) => definition.type === "tool_search"
      ? "tool_search"
      : definition.name),
    ["tool_search"],
  );
  assert.doesNotMatch(mcp.definitions()[0].description, /account|Connected/);
  assert.equal(mcp.resolve(name), undefined);
  const hidden = mcp.search({ query: "account records" });
  assert.deepEqual(hidden.output.tools, []);
  assert.deepEqual(hidden.structuredResult, []);

  available = true;
  assert.deepEqual(
    mcp.definitions().map((definition) => definition.type === "tool_search"
      ? "tool_search"
      : definition.name),
    ["tool_search"],
  );
  await mcp.settled();
  assert.equal(listCalls, 1);
  assert.deepEqual(
    mcp.definitions().map((definition) => definition.type === "tool_search"
      ? "tool_search"
      : definition.name),
    ["tool_search", name],
  );
  assert.match(mcp.definitions()[0].description, /account: Connected account tools/);
  const visible = mcp.search({ query: "account records" });
  assert.equal(visible.output.tools[0].name, name);
  assert.equal(visible.structuredResult[0].name, "mcp__account__");
  const resolved = mcp.resolve(name);
  await resolved.handler({ id: "record-1" });
  assert.deepEqual(calls, [{ name: "lookup", arguments: { id: "record-1" } }]);

  available = false;
  assert.equal(mcp.resolve(name), undefined);
  await assert.rejects(
    resolved.handler({ id: "record-2" }),
    /MCP server account is unavailable/,
  );
  assert.equal(calls.length, 1);
});

test("MCP initialization retries after authorization ends during discovery", async () => {
  let available = true;
  let attempts = 0;
  let releaseFirst;
  let markStarted;
  const firstStarted = new Promise((resolve) => { markStarted = resolve; });
  const firstBlocked = new Promise((resolve) => { releaseFirst = resolve; });
  const mcp = await createMcpRuntime({
    account: {
      isAvailable: () => available,
      client: {
        async listTools() {
          attempts += 1;
          if (attempts === 1) {
            markStarted();
            await firstBlocked;
            if (!available) throw new Error("authorization ended during discovery");
          }
          return { tools: [{ name: "lookup", inputSchema: { type: "object" } }] };
        },
        async callTool() { return { content: [] }; },
      },
    },
  });

  await firstStarted;
  available = false;
  releaseFirst();
  await mcp.settled();
  assert.equal(attempts, 1);

  available = true;
  mcp.definitions();
  await mcp.settled();
  assert.equal(attempts, 2);
  assert.ok(mcp.resolve("mcp__account__lookup"));
});

test("dynamically authorized MCP initialization retries a transient failure", async () => {
  let attempts = 0;
  const mcp = await createMcpRuntime({
    account: {
      isAvailable: () => true,
      client: {
        async listTools() {
          attempts += 1;
          if (attempts === 1) throw new Error("temporary broker outage");
          return { tools: [{ name: "lookup", inputSchema: { type: "object" } }] };
        },
        async callTool() { return { content: [] }; },
      },
    },
  });

  await mcp.settled();
  assert.equal(attempts, 1);
  mcp.definitions();
  await mcp.settled();
  assert.equal(attempts, 2);
  assert.ok(mcp.resolve("mcp__account__lookup"));
});

test("MCP server availability requires a synchronous boolean guard", async () => {
  const client = {
    async listTools() { return { tools: [] }; },
    async callTool() { return { content: [] }; },
  };
  await assert.rejects(
    createMcpRuntime({ invalid: { client, isAvailable: true } }),
    /MCP server invalid isAvailable must be a function/,
  );

  await assert.rejects(
    createMcpRuntime({ invalid: { client, isAvailable: async () => true } }),
    /MCP server invalid isAvailable must return boolean/,
  );
});

test("remote MCP failures are reported by tool_search without breaking agent creation", async () => {
  const mcp = await createMcpRuntime({
    unavailable: {
      client: {
        async listTools() { throw new Error("connection refused"); },
        async callTool() { throw new Error("unreachable"); },
      },
    },
  });
  await mcp.settled();
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);
  const result = JSON.parse(await runtime.executeTool(
    "tool_search",
    JSON.stringify({ query: "anything" }),
  ));
  assert.equal(result.success, true);
  assert.deepEqual(JSON.parse(result.output).failed_servers, {
    unavailable: "connection refused",
  });
});

test("MCP catalog identity can bind each resolved tool to its exact hosted provider", async () => {
  const connectionId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const mcp = await createMcpRuntime({
    [connectionId]: {
      client: {
        async listTools() {
          return { tools: [{ name: "lookup", inputSchema: { type: "object" } }] };
        },
        async callTool() { return { content: [] }; },
      },
    },
  }, {
    catalogProvider: (serverName) => `mcp:${serverName}`,
  });
  await mcp.settled();

  const tool = mcp.resolve(`mcp__${connectionId}__lookup`);
  assert.equal(tool.provider, `mcp:${connectionId}`);
  assert.equal(tool.remoteName, "lookup");
  await mcp.close();

  await assert.rejects(
    createMcpRuntime({ fixture: { client: { async listTools() { return { tools: [] }; } } } }, {
      catalogProvider: "mcp:fixture",
    }),
    /catalogProvider must be a function/,
  );
});

test("MCP discovery runs behind agent readiness and reports pending catalogs", async () => {
  let finishDiscovery;
  const discovery = new Promise((resolve) => { finishDiscovery = resolve; });
  const mcp = await createMcpRuntime({
    docs: {
      client: {
        listTools: () => discovery,
      },
    },
  });
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);

  const pending = JSON.parse(await runtime.executeTool(
    "tool_search",
    JSON.stringify({ query: "documentation" }),
  ));
  assert.equal(JSON.parse(pending.output).pending_servers, 1);

  finishDiscovery({
    tools: [{
      name: "search_docs",
      description: "Search documentation.",
      inputSchema: { type: "object" },
    }],
  });
  await mcp.settled();
  const ready = JSON.parse(await runtime.executeTool(
    "tool_search",
    JSON.stringify({ query: "documentation" }),
  ));
  assert.equal(JSON.parse(ready.output).pending_servers, 0);
  assert.equal(ready.structured_result[0].tools[0].name, "search_docs");
});

test("MCP startup timeout bounds complete paginated discovery", async () => {
  let pages = 0;
  const mcp = await createMcpRuntime({
    slow: {
      startupTimeoutMs: 20,
      client: {
        async listTools(_params, options) {
          pages += 1;
          if (pages === 1) return { tools: [], nextCursor: "next" };
          await new Promise((_resolve, reject) => {
            options.signal.addEventListener(
              "abort",
              () => reject(new Error("aborted")),
              { once: true },
            );
          });
        },
      },
    },
  });
  await mcp.settled();
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);
  const result = JSON.parse(await runtime.executeTool(
    "tool_search",
    JSON.stringify({ query: "slow" }),
  ));
  assert.match(
    JSON.parse(result.output).failed_servers.slow,
    /startup exceeded 20 milliseconds/,
  );
  assert.equal(pages, 2);
});

test("MCP tool deadlines settle even when an injected client ignores AbortSignal", async () => {
  const mcp = await createMcpRuntime({
    stubborn: {
      timeoutMs: 10,
      client: {
        async listTools() {
          return {
            tools: [{
              name: "hang",
              description: "Never settles.",
              inputSchema: { type: "object", additionalProperties: false },
            }],
          };
        },
        callTool() {
          return new Promise(() => {});
        },
      },
    },
  });
  await mcp.settled();
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);
  const started = Date.now();
  const result = JSON.parse(await runtime.executeTool(
    "mcp__stubborn__hang",
    "{}",
  ));
  assert.equal(result.success, false);
  assert.match(result.output, /exceeded 10 milliseconds/);
  assert.ok(Date.now() - started < 1_000);
  await mcp.close();
});

test("remote MCP tools retry payment challenges through McpClient.wrap", async () => {
  const challenge = Challenge.from({
    id: "nanocodex-paid-mcp",
    intent: "charge",
    method: "tempo",
    realm: "mercator.tempo.xyz",
    request: {},
  });
  const calls = [];
  let credentials = 0;
  const client = {
    async listTools() {
      return {
        tools: [{ name: "premium", inputSchema: { type: "object" } }],
      };
    },
    async callTool(params) {
      calls.push(params);
      if (calls.length === 1) {
        throw new McpError(Mcp.paymentRequiredCode, "Payment Required", {
          challenges: [challenge],
          httpStatus: 402,
        });
      }
      assert.ok(params._meta?.[Mcp.credentialMetaKey]);
      return {
        content: [{ type: "text", text: "paid MCP result" }],
        _meta: {
          [Mcp.receiptMetaKey]: {
            method: "tempo",
            reference: "0xreceipt",
            status: "success",
            timestamp: new Date().toISOString(),
          },
        },
      };
    },
  };
  const method = Method.toClient(Methods.charge, {
    async createCredential({ challenge: selected }) {
      credentials += 1;
      return Credential.serialize({
        challenge: selected,
        payload: { signature: "0xsignature", type: "transaction" },
      });
    },
  });
  const mcp = await createMcpRuntime({
    mercator: {
      client,
      payment: { methods: [method] },
    },
  });
  await mcp.settled();

  const result = await mcp.resolve("mcp__mercator__premium").handler({});
  assert.equal(credentials, 1);
  assert.equal(calls.length, 2);
  assert.equal(result.value.content[0].text, "paid MCP result");
  assert.equal(result.value.receipt.status, "success");
});

test("MCP isError is a failed tool result with server and remote-tool provenance", async () => {
  const mcp = await createMcpRuntime({
    fixture: {
      client: {
        async listTools() {
          return {
            tools: [{
              name: "fail",
              annotations: { readOnlyHint: true },
              inputSchema: { type: "object" },
            }],
          };
        },
        async callTool() {
          return {
            content: [{ type: "text", text: "remote failure" }],
            isError: true,
          };
        },
      },
    },
  });
  await mcp.settled();
  const runtime = createCodeRuntime();
  runtime.addProvider(mcp);

  const direct = JSON.parse(await runtime.executeTool(
    "mcp__fixture__fail",
    "{}",
    "mcp-failure",
    "direct-failure",
  ));
  assert.equal(direct.success, false);
  assert.deepEqual(direct.metadata, { mcp_server: "fixture", mcp_tool: "fail" });
  assert.equal(JSON.parse(direct.output).isError, true);

  const nested = JSON.parse(await runtime.executeCode(`
    try { await tools.mcp__fixture__fail({}); } catch (error) { text(error.isError); }
  `, "mcp-failure", "nested-failure"));
  assert.equal(nested.success, true);
  assert.equal(nested.nested_calls[0].success, false);
  assert.deepEqual(nested.nested_calls[0].metadata, {
    mcp_server: "fixture",
    mcp_tool: "fail",
  });
  assert.match(JSON.stringify(nested.output), /true/);
});

test("MCP parallel safety requires an annotation or explicit server policy", async () => {
  const client = {
    async listTools() {
      return { tools: [
        { name: "default", inputSchema: { type: "object" } },
        { name: "allowlisted", inputSchema: { type: "object" } },
        {
          name: "annotated",
          annotations: { readOnlyHint: true },
          inputSchema: { type: "object" },
        },
      ] };
    },
  };
  const selective = await createMcpRuntime({
    selective: { client, parallelTools: ["allowlisted"] },
  });
  const global = await createMcpRuntime({
    global: { client, supportsParallelToolCalls: true },
  });
  await Promise.all([selective.settled(), global.settled()]);

  assert.equal(selective.resolve("mcp__selective__default").parallelSafe, false);
  assert.equal(selective.resolve("mcp__selective__allowlisted").parallelSafe, true);
  assert.equal(selective.resolve("mcp__selective__annotated").parallelSafe, true);
  assert.equal(global.resolve("mcp__global__default").parallelSafe, true);
});
