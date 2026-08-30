import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import WebSocket, { WebSocketServer } from "ws";

import { Agent, Transport } from "../host/index.mjs";
import { artifact } from "../tools/artifact.mjs";
import { bindBrowser } from "../tools/browser/index.mjs";
import * as datasets from "../tools/dataset.mjs";
import * as standard from "../tools/standard.mjs";

const SUBAGENT_TOOL_NAMES = Object.freeze([
  "submit_result",
  "spawn_agent",
  "send_agent_message",
  "list_agents",
  "wait_agent",
  "interrupt_agent",
  "close_agent",
]);

const SHELL_DESCRIPTOR = Object.freeze({
  shell: "nanocodex-just-bash",
  commands: Object.freeze(["curl", "gh", "git", "python3"]),
  customCommands: Object.freeze(["gh", "git", "python3"]),
  cwd: "/workspace",
  limits: Object.freeze({ maxFileSystemBytes: 256 * 1024 * 1024 }),
  network: Object.freeze({ enabled: true, mode: "connector-http-gateway" }),
  pty: false,
  sessions: false,
  sandboxEscalation: false,
});

const createWarmAgent = ({
  apiKey,
  createWebSocket,
  WebSocketImpl,
  websocketUrl,
  ...options
}) => Agent.create({
  ...options,
  codeEvaluator: options.codeEvaluator ?? evaluateInTestRealm,
  transport: Transport.openAi({
    apiKey,
    createWebSocket,
    WebSocketImpl,
    websocketUrl,
    websocketWarmup: true,
  }),
});

async function evaluateInTestRealm(source, environment) {
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const script = new AsyncFunction(
    "tools", "ALL_TOOLS", "text", "image", "generatedImage", "store", "load", "exit",
    "require", "console", source,
  );
  await script(
    environment.tools,
    environment.toolDefinitions,
    environment.text,
    environment.image,
    environment.generatedImage,
    environment.store,
    environment.load,
    environment.exit,
    environment.require,
    environment.console,
  );
}

test("web-target WASM runs the shared model loop through the browser host", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  let agent;
  let branch;
  let watch;
  try {
    const connection = new Promise((resolve) => server.once("connection", resolve));
    const events = [];
    const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    const endpoint = `ws://127.0.0.1:${server.address().port}`;
    agent = await createWarmAgent({
      apiKey: "test-key",
      WebSocketImpl: WebSocket,
      module: wasm,
      websocketUrl: endpoint,
      thinking: "low",
      sessionId: "018f1f9a-7b3c-7a07-8000-000000000007",
      executionEnvironment: {
        currentDate: "2026-08-18",
        timezone: "America/Los_Angeles",
        projectInstructions: "BROWSER_PROJECT_INSTRUCTIONS",
      },
    });
    watch = agent.events.watch({ includeAllSessions: true });
    watch.onEvent((event) => events.push(event));

    const scenario = (async () => {
      const socket = await connection;
      const reader = messageReader(socket);
      await reader.next();
      send(socket, { type: "response.completed", response: { id: "web-warmup", usage: null } });
      const generation = await reader.next();
      assert.match(JSON.stringify(generation.input), /BROWSER_PROJECT_INSTRUCTIONS/);
      assert.equal(generation.previous_response_id, "web-warmup");
      send(socket, {
        type: "response.completed",
        response: {
          id: "web-final",
          status: "completed",
          output: [{
            type: "message",
            role: "assistant",
            content: [{ type: "output_text", text: "WEB_WASM_OK" }],
          }],
          usage: null,
        },
      });
    })();

    const [result] = await Promise.all([
      agent.turn.prompt({ input: "Reply with WEB_WASM_OK." }).result(),
      scenario,
    ]);
    assert.equal(result.finalMessage, "WEB_WASM_OK");

    const branchConnection = new Promise((resolve) => server.once("connection", resolve));
    branch = await agent.session.fork();
    assert.notEqual(branch.sessionId, agent.sessionId);
    assert.throws(
      () => branch.turn.prompt({
        input: [{ type: "local_image", path: "/private/model-input.png" }],
      }),
      /cannot reference local filesystem paths/,
    );
    assert.throws(
      () => branch.turn.prompt({
        input: [{ type: "local_audio", path: "/private/model-input.wav" }],
      }),
      /cannot reference local filesystem paths/,
    );
    const branchTurn = branch.turn.prompt({ input: [
      { type: "image", image_url: "data:image/png;base64,iVBORw0KGgo=" },
      { type: "text", text: "Reply with WEB_FORK_OK." },
    ] });
    const branchSocket = await branchConnection;
    const branchReader = messageReader(branchSocket);
    const branchRequest = await branchReader.next();
    assert.equal(branchRequest.previous_response_id, "web-final");
    const delta = JSON.stringify(branchRequest.input);
    assert.doesNotMatch(delta, /Reply with WEB_WASM_OK/);
    assert.doesNotMatch(delta, /WEB_WASM_OK/);
    assert.match(delta, /WEB_FORK_OK/);
    assert.match(delta, /input_image/);
    send(branchSocket, {
      type: "response.completed",
      response: {
        id: "web-branch-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "WEB_FORK_OK" }],
        }],
        usage: null,
      },
    });
    assert.equal((await branchTurn.result()).finalMessage, "WEB_FORK_OK");
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(events.filter((event) => event.type === "run.completed").length, 2);
  } finally {
    watch?.off();
    branch?.dispose();
    agent?.dispose();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test("web-target WASM directly dispatches a CSP-safe application tool", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const events = [];
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const agent = await createWarmAgent({
    apiKey: "test-key",
    WebSocketImpl: WebSocket,
    module: wasm,
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000008",
    thinking: "low",
    toolMode: "direct",
    tools: {
      runtimeInfo: {
        description: "Return the runtime.",
        parameters: { type: "object", additionalProperties: false },
        handler: () => ({ runtime: "worker" }),
      },
    },
    websocketUrl: `ws://127.0.0.1:${server.address().port}`,
  });
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event));
  try {
    const turn = agent.turn.prompt({ input: "Call runtimeInfo." });
    const socket = await connection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    const toolPrefix = warmup.input.find((item) => item.type === "additional_tools");
    assert.deepEqual(toolPrefix.tools.map((tool) => tool.name), [
      "close_agent",
      "interrupt_agent",
      "list_agents",
      "runtimeInfo",
      "send_agent_message",
      "spawn_agent",
      "submit_result",
      "wait_agent",
    ]);
    send(socket, { type: "response.completed", response: { id: "direct-warmup", usage: null } });
    const generation = await reader.next();
    assert.equal(generation.previous_response_id, "direct-warmup");
    send(socket, {
      type: "response.completed",
      response: {
        id: "direct-tool",
        status: "completed",
        output: [{
          type: "function_call",
          call_id: "call-runtime",
          name: "runtimeInfo",
          arguments: "{}",
        }],
        usage: null,
      },
    });
    const continuation = await reader.next();
    assert.equal(continuation.input[0].type, "function_call_output");
    assert.equal(continuation.input[0].call_id, "call-runtime");
    assert.deepEqual(JSON.parse(continuation.input[0].output), { runtime: "worker" });
    send(socket, {
      type: "response.completed",
      response: {
        id: "direct-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "worker" }],
        }],
        usage: null,
      },
    });
    assert.equal((await turn.result()).finalMessage, "worker");
    assert.equal(events.some((event) =>
      event.type === "tool.call" && event.payload.tool === "runtimeInfo"), true);
    assert.equal(events.some((event) =>
      event.type === "tool.result" && event.payload.status === "completed"), true);
  } finally {
    watch.off();
    agent.dispose();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test("web-target WASM exposes browser bash and Rust apply_patch as standard tools", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const files = new Map([["/workspace/note.txt", new TextEncoder().encode("before\n")]]);
  const workspace = {
    root: "/workspace",
    async list() { return []; },
    async readFile(path) {
      const value = files.get(path.startsWith("/") ? path : `/workspace/${path}`);
      if (!value) throw Object.assign(new Error("not found"), { code: "ENOENT" });
      return value;
    },
    async writeFile(path, contents) {
      files.set(path.startsWith("/") ? path : `/workspace/${path}`, typeof contents === "string"
        ? new TextEncoder().encode(contents)
        : new Uint8Array(contents.buffer ?? contents, contents.byteOffset ?? 0, contents.byteLength));
    },
    async remove(path) {
      const resolved = path.startsWith("/") ? path : `/workspace/${path}`;
      if (!files.delete(resolved)) throw Object.assign(new Error("not found"), { code: "ENOENT" });
    },
    async mkdir() {},
  };
  const agent = await createWarmAgent({
    apiKey: "test-key",
    WebSocketImpl: WebSocket,
    module: wasm,
    filesystem: workspace,
    filesystemTools: false,
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000010",
    thinking: "low",
    tools: {
      exec_command: {
        description: "Run browser bash.",
        parameters: { type: "object", required: ["cmd"] },
        outputSchema: {
          type: "object",
          properties: {
            output: { type: "string" },
            wall_time_seconds: { type: "number" },
          },
          required: ["output", "wall_time_seconds"],
          additionalProperties: false,
        },
        handler: () => ({ output: "", wall_time_seconds: 0, exit_code: 0 }),
      },
    },
    websocketUrl: `ws://127.0.0.1:${server.address().port}`,
  });
  try {
    const turn = agent.turn.prompt({ input: "Update note.txt with apply_patch." });
    const socket = await connection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    const toolPrefix = warmup.input.find((item) => item.type === "additional_tools");
    assert.deepEqual(toolPrefix.tools.map((tool) => tool.name), [
      "exec",
      "exec_command",
      "apply_patch",
      ...SUBAGENT_TOOL_NAMES,
    ]);
    const execCommand = toolPrefix.tools.find((tool) => tool.name === "exec_command");
    assert.match(execCommand.description, /^Run browser bash\./);
    assert.match(execCommand.description, /output: string/);
    assert.equal(toolPrefix.tools.some((tool) => tool.name === "read_file"), false);
    send(socket, { type: "response.completed", response: { id: "workspace-warmup", usage: null } });
    await reader.next();
    send(socket, {
      type: "response.completed",
      response: {
        id: "workspace-patch",
        status: "completed",
        output: [{
          type: "custom_tool_call",
          call_id: "call-apply-patch",
          name: "apply_patch",
          input: "*** Begin Patch\n*** Update File: note.txt\n@@\n-before\n+after\n*** End Patch",
        }],
        usage: null,
      },
    });
    const continuation = await reader.next();
    assert.equal(continuation.input[0].type, "custom_tool_call_output");
    assert.equal(continuation.input[0].call_id, "call-apply-patch");
    assert.match(continuation.input[0].output, /Success.*M note\.txt/s);
    assert.equal(new TextDecoder().decode(files.get("/workspace/note.txt")), "after\n");
    send(socket, {
      type: "response.completed",
      response: {
        id: "workspace-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "updated" }],
        }],
        usage: null,
      },
    });
    assert.equal((await turn.result()).finalMessage, "updated");
  } finally {
    agent.dispose();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test("web-target WASM keeps remote MCP deferred behind tool_search and Code Mode", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const events = [];
  const calls = [];
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const mcpClient = {
    async listTools() {
      return {
        tools: [{
          name: "echo",
          description: "Echo a deterministic MCP fixture message.",
          inputSchema: {
            type: "object",
            properties: { message: { type: "string" } },
            required: ["message"],
            additionalProperties: false,
          },
        }],
      };
    },
    async callTool({ name, arguments: input }) {
      calls.push({ name, input });
      return {
        content: [{ type: "text", text: `fixture:${input.message}` }],
        isError: false,
      };
    },
  };
  const agent = await createWarmAgent({
    apiKey: "test-key",
    WebSocketImpl: WebSocket,
    module: wasm,
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000009",
    thinking: "low",
    mcp: {
      fixture: {
        client: mcpClient,
        description: "Deterministic remote MCP fixture.",
      },
    },
    websocketUrl: `ws://127.0.0.1:${server.address().port}`,
  });
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event));
  let turn;
  try {
    turn = agent.turn.prompt({ input: "Find and call the remote MCP echo tool." });
    const socket = await connection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    const toolPrefix = warmup.input.find((item) => item.type === "additional_tools");
    assert.deepEqual(toolPrefix.tools.map((tool) => tool.name ?? tool.type), [
      "exec",
      "tool_search",
      ...SUBAGENT_TOOL_NAMES,
    ]);
    assert.doesNotMatch(toolPrefix.tools[0].description, /mcp__fixture__echo/);
    assert.equal(toolPrefix.tools.some((tool) => tool.name === "mcp__fixture__echo"), false);
    send(socket, { type: "response.completed", response: { id: "mcp-warmup", usage: null } });

    const generation = await reader.next();
    assert.equal(generation.previous_response_id, "mcp-warmup");
    send(socket, {
      type: "response.completed",
      response: {
        id: "mcp-search",
        status: "completed",
        output: [{
          type: "tool_search_call",
          call_id: "search-mcp",
          execution: "client",
          arguments: { query: "echo deterministic message", limit: 1 },
        }],
        usage: null,
      },
    });

    const searched = await reader.next();
    assert.equal(searched.previous_response_id, "mcp-search");
    assert.equal(searched.input[0].type, "tool_search_output");
    assert.equal(searched.input[0].tools[0].type, "namespace");
    assert.equal(searched.input[0].tools[0].name, "mcp__fixture__");
    assert.deepEqual(searched.input[0].tools[0].tools.map((tool) => tool.name), ["echo"]);
    send(socket, {
      type: "response.completed",
      response: {
        id: "mcp-empty-search",
        status: "completed",
        output: [{
          type: "tool_search_call",
          call_id: "search-mcp-empty",
          execution: "client",
          arguments: { query: "what mcps do u got", limit: 8 },
        }],
        usage: null,
      },
    });

    const emptySearched = await reader.next();
    assert.equal(emptySearched.previous_response_id, "mcp-empty-search");
    assert.equal(emptySearched.input.length, 1);
    assert.equal(emptySearched.input[0].type, "tool_search_output");
    assert.equal(emptySearched.input[0].call_id, "search-mcp-empty");
    assert.deepEqual(emptySearched.input[0].tools, []);
    send(socket, {
      type: "response.completed",
      response: {
        id: "mcp-exec",
        status: "completed",
        output: [{
          type: "custom_tool_call",
          call_id: "call-exec-mcp",
          name: "exec",
          input: `const found = await tools.tool_search({ query: "echo deterministic message", limit: 1 });
const selected = found.tools[0];
text(await tools[selected.name]({ message: "hello" }));`,
        }],
        usage: null,
      },
    });

    const called = await reader.next();
    assert.equal(called.previous_response_id, "mcp-exec");
    assert.equal(called.input[0].type, "custom_tool_call_output");
    assert.match(JSON.stringify(called.input[0].output), /fixture:hello/);
    send(socket, {
      type: "response.completed",
      response: {
        id: "mcp-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "MCP_WASM_OK" }],
        }],
        usage: null,
      },
    });

    assert.equal((await turn.result()).finalMessage, "MCP_WASM_OK");
    assert.deepEqual(calls, [{ name: "echo", input: { message: "hello" } }]);
    assert.equal(events.some((event) =>
      event.type === "tool.call" && event.payload.tool === "mcp__fixture__echo"), true);
  } finally {
    turn?.dispose();
    watch.off();
    await agent.session.shutdown();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test("web-target WASM directly dispatches a discovered pure-attached tool", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const calls = [];
  const provider = {
    definitions: () => [{
      type: "function",
      name: "browser_echo",
      description: "Echo a deterministic browser fixture message.",
      strict: false,
      defer_loading: true,
      parameters: {
        type: "object",
        properties: { message: { type: "string" } },
        required: ["message"],
        additionalProperties: false,
      },
    }],
    resolve: (name) => name === "browser_echo" ? {
      name,
      parallelSafe: false,
      handler: (input) => {
        calls.push(input);
        return { echoed: input.message, source: "browser-host" };
      },
    } : undefined,
  };
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const agent = await createWarmAgent({
    apiKey: "test-key",
    WebSocketImpl: WebSocket,
    module: wasm,
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000013",
    thinking: "low",
    [Symbol.for("nanocodex.browser.internalRuntime")]: { toolProviders: [provider] },
    websocketUrl: `ws://127.0.0.1:${server.address().port}`,
  });
  let turn;
  let result;
  try {
    turn = agent.turn.prompt({ input: "Find and call the browser echo tool." });
    const socket = await connection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    const toolPrefix = warmup.input.find((item) => item.type === "additional_tools");
    assert.equal(toolPrefix.tools.some((tool) => tool.name === "browser_echo"), false);
    send(socket, { type: "response.completed", response: { id: "attached-warmup", usage: null } });

    await reader.next();
    send(socket, {
      type: "response.completed",
      response: {
        id: "attached-search",
        status: "completed",
        output: [{
          type: "tool_search_call",
          call_id: "search-attached",
          execution: "client",
          arguments: { query: "browser echo deterministic message", limit: 1 },
        }],
        usage: null,
      },
    });

    const searched = await reader.next();
    assert.equal(searched.input[0].type, "tool_search_output");
    assert.equal(searched.input[0].tools[0].name, "browser_echo");
    send(socket, {
      type: "response.completed",
      response: {
        id: "attached-call",
        status: "completed",
        output: [{
          type: "function_call",
          call_id: "call-attached",
          name: "browser_echo",
          arguments: JSON.stringify({ message: "BROWSER_ECHO_OK" }),
        }],
        usage: null,
      },
    });

    const called = await reader.next();
    assert.equal(called.input[0].type, "function_call_output");
    assert.match(called.input[0].output, /BROWSER_ECHO_OK/);
    send(socket, {
      type: "response.completed",
      response: {
        id: "attached-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "ATTACHED_WASM_OK" }],
        }],
        usage: null,
      },
    });

    result = await turn.result();
    assert.equal(result.finalMessage, "ATTACHED_WASM_OK");
    assert.deepEqual(calls, [{ message: "BROWSER_ECHO_OK" }]);
  } finally {
    result?.dispose();
    turn?.dispose();
    await agent.session.shutdown();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test("web-target WASM executes the complete browser harness tool contract", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const png = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const datasetBytes = new TextEncoder().encode(
    '{"id":1,"label":"alpha"}\n{"id":2,"label":"beta"}\n',
  );
  const workspace = memoryWorkspace({
    "/workspace/note.txt": "before\n",
    "/workspace/pixel.png": png,
  });
  const effects = {
    artifacts: [],
    datasetRequests: [],
    hostCalls: [],
    images: [],
    mcp: [],
    rememberedImages: [],
    web: [],
  };
  const runtime = bindBrowser({
    datasets,
    origin: "https://demo.test",
    standard,
    threadId: "browser-harness-e2e",
    shell: {
      descriptor: SHELL_DESCRIPTOR,
      artifactTool: artifact({
        workspace,
        onArtifact: (document) => effects.artifacts.push(document),
      }),
      execTool: {
        description: "Run deterministic browser bash.",
        parameters: {
          type: "object",
          properties: { cmd: { type: "string" } },
          required: ["cmd"],
          additionalProperties: false,
        },
        outputSchema: {
          type: "object",
          properties: {
            exit_code: { type: "integer" },
            output: { type: "string" },
            wall_time_seconds: { type: "number" },
          },
          required: ["exit_code", "output", "wall_time_seconds"],
          additionalProperties: false,
        },
        handler: async ({ cmd }) => ({
          exit_code: 0,
          output: "browser-shell:" + cmd,
          wall_time_seconds: 0,
        }),
      },
      instructions: "deterministic browser harness",
      projectInstructions: "deterministic browser project",
      workspace,
    },
  }, {
    dataset: {
      fetch: objectFetch(
        new Map([["https://data.example/browser-tools.jsonl", datasetBytes]]),
        effects.datasetRequests,
      ),
    },
    images: {
      fetch: async (url, init) => {
        effects.images.push({ url: String(url), body: JSON.parse(init.body) });
        return Response.json({
          image_url: "data:image/png;base64,Z2VuZXJhdGVk",
          output_hint: "fixture image generated",
        });
      },
    },
    rememberImage: (sessionId, imageUrl) => {
      effects.rememberedImages.push({ sessionId, imageUrl });
    },
    web: {
      fetch: async (url, init) => {
        effects.web.push({ url: String(url), body: JSON.parse(init.body) });
        return Response.json({ output: "fixture web result" });
      },
    },
  });
  let lifecycleStarted;
  let releaseLifecycle;
  const lifecycleStart = new Promise((resolve) => { lifecycleStarted = resolve; });
  const lifecycleRelease = new Promise((resolve) => { releaseLifecycle = resolve; });
  const tools = [...runtime.tools.map((tool) => Object.freeze({
    ...tool,
    async handler(input, context) {
      effects.hostCalls.push({
        name: tool.name,
        input: structuredClone(input),
        parentCallId: context.parentCallId,
      });
      return tool.handler(input, context);
    },
  })), Object.freeze({
    name: "lifecycle_probe",
    description: "Wait until the browser test releases this lifecycle probe.",
    parameters: { type: "object", additionalProperties: false },
    async handler() {
      lifecycleStarted();
      await lifecycleRelease;
      return { released: true };
    },
  })];
  const mcpClient = {
    async listTools() {
      return {
        tools: [
          {
            name: "echo",
            description: "Echo one deterministic fixture message.",
            annotations: { readOnlyHint: true },
            inputSchema: {
              type: "object",
              properties: { message: { type: "string" } },
              required: ["message"],
              additionalProperties: false,
            },
          },
          {
            name: "fail",
            description: "Return one deterministic MCP failure.",
            inputSchema: { type: "object", additionalProperties: false },
          },
        ],
      };
    },
    async callTool({ name, arguments: input }) {
      effects.mcp.push({ name: "callTool", input: { name, arguments: input } });
      if (name === "fail") {
        return {
          content: [{ type: "text", text: "fixture remote failure" }],
          isError: true,
        };
      }
      return {
        content: [{ type: "text", text: "fixture:" + input.message }],
        isError: false,
      };
    },
  };
  const events = [];
  const agent = await createWarmAgent({
    apiKey: "test-key",
    WebSocketImpl: WebSocket,
    module: wasm,
    filesystem: runtime.filesystem,
    filesystemTools: false,
    instructions: runtime.instructions,
    executionEnvironment: {
      currentDate: "2026-08-18",
      timezone: "Europe/Athens",
      projectInstructions: runtime.projectInstructions,
    },
    mcp: {
      fixture: {
        client: mcpClient,
        description: "Deterministic MCP resources and deferred tools.",
      },
    },
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000011",
    thinking: "low",
    tools,
    websocketUrl: "ws://127.0.0.1:" + server.address().port,
  });
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event));
  let turn;
  try {
    turn = agent.turn.prompt({ input: "Exercise the configured browser tools." });
    const socket = await connection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    const toolPrefix = warmup.input.find((item) => item.type === "additional_tools");
    assert.deepEqual(toolPrefix.tools.map((tool) => tool.name ?? tool.type), [
      "exec",
      "exec_command",
      "update_plan",
      "apply_patch",
      "view_image",
      "tool_search",
      ...SUBAGENT_TOOL_NAMES,
    ]);
    assert.equal(toolPrefix.tools[0].type, "custom");
    assert.equal(
      toolPrefix.tools.find((tool) => (tool.name ?? tool.type) === "tool_search")?.type,
      "tool_search",
    );
    send(socket, { type: "response.completed", response: { id: "combined-warmup", usage: null } });

    const generation = await reader.next();
    assert.equal(generation.previous_response_id, "combined-warmup");
    send(socket, {
      type: "response.completed",
      response: {
        id: "combined-direct",
        status: "completed",
        output: [
          {
            type: "function_call",
            call_id: "call-shell",
            name: "exec_command",
            arguments: '{"cmd":"pwd"}',
          },
          {
            type: "function_call",
            call_id: "call-plan",
            name: "update_plan",
            arguments: '{"explanation":"fixture","plan":[{"step":"exercise tools","status":"completed"}]}',
          },
          {
            type: "custom_tool_call",
            call_id: "call-patch",
            name: "apply_patch",
            input: "*** Begin Patch\n*** Update File: note.txt\n@@\n-before\n+after\n*** End Patch",
          },
          {
            type: "function_call",
            call_id: "call-view",
            name: "view_image",
            arguments: '{"path":"/workspace/pixel.png","detail":"original"}',
          },
        ],
        usage: null,
      },
    });

    const direct = await reader.next();
    assert.equal(direct.previous_response_id, "combined-direct");
    assert.deepEqual(direct.input.map(({ type, call_id }) => ({ type, call_id })), [
      { type: "function_call_output", call_id: "call-shell" },
      { type: "function_call_output", call_id: "call-plan" },
      { type: "custom_tool_call_output", call_id: "call-patch" },
      { type: "function_call_output", call_id: "call-view" },
    ]);
    assert.deepEqual(JSON.parse(direct.input[0].output), {
      exit_code: 0,
      output: "browser-shell:pwd",
      wall_time_seconds: 0,
    });
    assert.deepEqual(JSON.parse(direct.input[1].output), { updated: true });
    assert.match(direct.input[2].output, /Success.*M note\.txt/s);
    assert.deepEqual(direct.input[3].output, [{
      type: "input_image",
      image_url: "data:image/png;base64,iVBORw0KGgo=",
    }]);
    assert.equal(await workspace.readText("/workspace/note.txt"), "after\n");

    send(socket, {
      type: "response.completed",
      response: {
        id: "combined-search",
        status: "completed",
        output: [{
          type: "tool_search_call",
          call_id: "call-search",
          execution: "client",
          arguments: { query: "deterministic fixture echo", limit: 1 },
        }],
        usage: null,
      },
    });
    const searched = await reader.next();
    assert.equal(searched.previous_response_id, "combined-search");
    assert.deepEqual(searched.input.map(({ type, call_id }) => ({ type, call_id })), [{
      type: "tool_search_output",
      call_id: "call-search",
    }]);
    assert.deepEqual(
      searched.input[0].tools.map((namespace) => ({
        name: namespace.name,
        tools: namespace.tools.map((tool) => tool.name),
      })),
      [{ name: "mcp__fixture__", tools: ["echo"] }],
    );

    const code = [
      "await tools.lifecycle_probe({});",
      "const patched = await tools.apply_patch(\"*** Begin Patch\\n*** Update File: note.txt\\n@@\\n-after\\n+nested\\n*** End Patch\");",
      'const viewed = await tools.view_image({ path: "/workspace/pixel.png", detail: "original" });',
      'const web = await tools.web__run({ search_query: [{ q: "browser tools" }] });',
      'const generated = await tools.image_gen__imagegen({ prompt: "fixture image" });',
      "generatedImage(generated);",
      "const opened = await tools.dataset({ operation: \"open\", source: { kind: \"url\", url: \"https://data.example/browser-tools.jsonl\", format: \"jsonl\" } });",
      "const queried = await tools.dataset({ operation: \"query\", dataset_id: opened.datasetId, columns: [\"id\"], filters: [{ column: \"label\", op: \"eq\", value: \"beta\" }], limit: 1 });",
      "const closed = await tools.dataset({ operation: \"close\", dataset_id: opened.datasetId });",
      "const rendered = await tools.render_artifact({ id: \"combined\", title: \"Combined\", source: \"function App() { return React.createElement('main', null, 'combined'); }\" });",
      "const remote = await tools.mcp__fixture__echo({ message: \"nested\" });",
      "let remoteFailed = false; try { await tools.mcp__fixture__fail({}); } catch (error) { remoteFailed = error.isError === true; }",
      "text(JSON.stringify({ patched: patched.includes('M note.txt'), viewed: viewed.detail, web, image: generated.image_url, rows: queried.rows, closed: closed.closed, rendered, remote: remote.content[0].text, remoteFailed }));",
    ].join("\n");
    send(socket, {
      type: "response.completed",
      response: {
        id: "combined-exec",
        status: "completed",
        output: [{
          type: "custom_tool_call",
          call_id: "call-exec",
          name: "exec",
          input: code,
        }],
        usage: null,
      },
    });

    await lifecycleStart;
    await waitFor(() => events.some((event) =>
      event.type === "tool.call" && event.payload.tool === "lifecycle_probe"));
    assert.equal(events.some((event) =>
      event.type === "tool.result" && event.payload.tool === "lifecycle_probe"), false);
    releaseLifecycle();

    const executed = await reader.next();
    assert.equal(executed.previous_response_id, "combined-exec");
    assert.deepEqual(executed.input.map(({ type, call_id }) => ({ type, call_id })), [{
      type: "custom_tool_call_output",
      call_id: "call-exec",
    }]);
    assert(Array.isArray(executed.input[0].output));
    assert.deepEqual(
      executed.input[0].output.find((item) => item.type === "input_image"),
      {
        type: "input_image",
        image_url: "data:image/png;base64,Z2VuZXJhdGVk",
      },
    );
    const summaryItem = executed.input[0].output.find((item) =>
      item.type === "input_text" && item.text.startsWith("{"));
    assert.deepEqual(JSON.parse(summaryItem.text), {
      patched: true,
      viewed: "original",
      web: "fixture web result",
      image: "data:image/png;base64,Z2VuZXJhdGVk",
      rows: [{ id: 2 }],
      closed: true,
      rendered: {
        artifactId: "combined",
        path: "/workspace/.nanocodex/artifacts/combined.json",
        title: "Combined",
        runtime: "react",
      },
      remote: "fixture:nested",
      remoteFailed: true,
    });
    assert.equal(await workspace.readText("/workspace/note.txt"), "nested\n");
    send(socket, {
      type: "response.completed",
      response: {
        id: "combined-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "COMBINED_TOOLS_OK" }],
        }],
        usage: null,
      },
    });

    const result = await turn.result();
    try {
      assert.equal(result.finalMessage, "COMBINED_TOOLS_OK");
    } finally {
      result.dispose();
    }
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(effects.hostCalls.map((call) => call.name), [
      "exec_command",
      "update_plan",
      "view_image",
      "view_image",
      "web__run",
      "image_gen__imagegen",
      "dataset",
      "dataset",
      "dataset",
      "render_artifact",
    ]);
    assert.deepEqual(
      effects.hostCalls.filter((call) => call.parentCallId === "call-exec")
        .map((call) => call.name),
      [
        "view_image",
        "web__run",
        "image_gen__imagegen",
        "dataset",
        "dataset",
        "dataset",
        "render_artifact",
      ],
    );
    assert.deepEqual(effects.mcp, [
      {
        name: "callTool",
        input: { name: "echo", arguments: { message: "nested" } },
      },
      {
        name: "callTool",
        input: { name: "fail", arguments: {} },
      },
    ]);
    assert.deepEqual(effects.web, [{
      url: "https://demo.test/api/tools/web-search",
      body: {
        commands: { search_query: [{ q: "browser tools" }] },
        session_id: agent.sessionId,
      },
    }]);
    assert.deepEqual(effects.images, [{
      url: "https://demo.test/api/tools/image-generation",
      body: { images: [], prompt: "fixture image" },
    }]);
    assert.deepEqual(effects.rememberedImages, [{
      sessionId: agent.sessionId,
      imageUrl: "data:image/png;base64,Z2VuZXJhdGVk",
    }]);
    assert.equal(effects.artifacts.length, 1);
    assert.equal(effects.artifacts[0].id, "combined");
    assert.deepEqual(
      JSON.parse(await workspace.readText("/workspace/.nanocodex/artifacts/combined.json")),
      effects.artifacts[0],
    );
    assert.ok(effects.datasetRequests.length >= 2);
    assert.deepEqual(
      events.filter((event) => event.type === "tool.call").map((event) => event.payload.tool),
      [
        "exec_command",
        "update_plan",
        "apply_patch",
        "view_image",
        "tool_search",
        "exec",
        "lifecycle_probe",
        "apply_patch",
        "view_image",
        "web__run",
        "image_gen__imagegen",
        "dataset",
        "dataset",
        "dataset",
        "render_artifact",
        "mcp__fixture__echo",
        "mcp__fixture__fail",
      ],
    );
    const toolResults = events.filter((event) => event.type === "tool.result");
    const remoteFailure = toolResults.find((event) => event.payload.tool === "mcp__fixture__fail");
    assert.equal(remoteFailure.payload.status, "failed");
    assert.deepEqual(remoteFailure.payload.metadata, {
      mcp_server: "fixture",
      mcp_tool: "fail",
    });
    assert.equal(
      toolResults.filter((event) => event !== remoteFailure)
        .every((event) => event.payload.status === "completed"),
      true,
    );
  } finally {
    turn?.dispose();
    watch.off();
    await agent.session.shutdown();
    for (const socket of server.clients) socket.terminate();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

function messageReader(socket) {
  const messages = [];
  let waiter;
  socket.on("message", (data) => {
    const value = JSON.parse(data.toString("utf8"));
    if (waiter) {
      const resolve = waiter;
      waiter = undefined;
      resolve(value);
    } else {
      messages.push(value);
    }
  });
  return {
    next() {
      if (messages.length) return Promise.resolve(messages.shift());
      return new Promise((resolve) => { waiter = resolve; });
    },
  };
}

function send(socket, value) {
  socket.send(JSON.stringify(value));
}

async function waitFor(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("condition was not observed before the deadline");
    await new Promise((resolve) => setImmediate(resolve));
  }
}

function memoryWorkspace(initial = {}) {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const files = new Map(Object.entries(initial).map(([path, contents]) => [
    resolveWorkspacePath(path),
    typeof contents === "string" ? encoder.encode(contents) : new Uint8Array(contents),
  ]));
  const directories = new Set(["/workspace"]);
  return {
    root: "/workspace",
    async list() {
      return [
        ...[...directories].filter((path) => path !== "/workspace")
          .map((path) => ({ kind: "directory", path })),
        ...[...files].map(([path, contents]) => ({
          kind: "file",
          path,
          size: contents.byteLength,
        })),
      ];
    },
    async readFile(path) {
      const contents = files.get(resolveWorkspacePath(path));
      if (!contents) throw Object.assign(new Error("not found"), { code: "ENOENT" });
      return contents;
    },
    async readText(path) {
      return decoder.decode(await this.readFile(path));
    },
    async writeFile(path, contents) {
      const bytes = typeof contents === "string"
        ? encoder.encode(contents)
        : contents instanceof ArrayBuffer
          ? new Uint8Array(contents)
          : new Uint8Array(contents.buffer, contents.byteOffset, contents.byteLength);
      files.set(resolveWorkspacePath(path), bytes);
    },
    async remove(path) {
      const resolved = resolveWorkspacePath(path);
      if (!files.delete(resolved)) throw Object.assign(new Error("not found"), { code: "ENOENT" });
    },
    async mkdir(path) {
      directories.add(resolveWorkspacePath(path));
    },
  };
}

function resolveWorkspacePath(path) {
  const value = String(path).replace(/^\.\//, "");
  return value.startsWith("/") ? value : "/workspace/" + value;
}

function objectFetch(objects, calls) {
  return async (input, init) => {
    const url = String(input);
    const bytes = objects.get(url);
    const method = init?.method ?? "GET";
    const range = new Headers(init?.headers).get("range");
    calls.push({ url, method, range });
    if (!bytes) return new Response("not found", { status: 404 });
    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: { "content-length": String(bytes.byteLength) },
      });
    }
    if (range) {
      const match = range.match(/^bytes=(\d+)-(\d+)$/);
      if (!match) return new Response("bad range", { status: 416 });
      const start = Number(match[1]);
      const end = Number(match[2]);
      const body = bytes.slice(start, end + 1);
      return new Response(body, {
        status: 206,
        headers: {
          "content-length": String(body.byteLength),
          "content-range": "bytes " + start + "-" + end + "/" + bytes.byteLength,
        },
      });
    }
    return new Response(bytes, {
      status: 200,
      headers: { "content-length": String(bytes.byteLength) },
    });
  };
}
