import { env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
} from "nanocodex-tools/execution-contract";
import { HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE } from "nanocodex-tools/hosted";
// @ts-expect-error The runtime subpath is intentionally JavaScript-only.
import { ToolRouter, toolMapSource } from "nanocodex-tools/runtime/tool-router";
import { createNamespaceExecutionTools } from "../src/namespace-tools";

import {
  AccountHostedTools,
  AccountHostedToolsProvider,
} from "../src/account-hosted-tools";

const ACCOUNT_A = "11111111-1111-4111-8111-111111111111";
const ACCOUNT_B = "22222222-2222-4222-8222-222222222222";
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");

const snapshot = {
  tools: [{
    provider: "fixture",
    remote_name: "lookup",
    parallel_safe: true,
    summary: "Fixture lookup",
    timeout_ms: 10_000,
    route_token: "route-token-a",
    definition: {
      type: "function" as const,
      name: "fixture__lookup",
      description: "Look up a fixture.",
      strict: true,
      parameters: { type: "object", additionalProperties: false },
      defer_loading: true as const,
    },
  }],
  machines: [{
    online: true,
    machine: {
      id: "laptop",
      name: "Build laptop",
      workspace: "/work/nanocodex",
      capabilities: ["filesystem", "native-shell"],
    },
    tools: [{
      name: "exec_command" as const,
      parallel_safe: true,
      route_token: "machine-route-token-a",
    }],
  }],
};

describe("account Hosted Tools provider", () => {
  it("settles an offline VM probe through the real broker and tool router, then recovers after reconnect", async () => {
    const namespace = (env as unknown as {
      NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
    }).NANOCODEX_ACCOUNT_TOOLS;
    const owner = crypto.randomUUID();
    const stub = namespace.getByName(owner);
    const attach = async () => {
      const response = await stub.fetch("https://account-tools.internal/tool-host", {
        headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
      });
      const socket = response.webSocket!;
      socket.accept();
      const ready = nextFrame(socket);
      socket.send(JSON.stringify({
        type: "catalog", attachment_id: "desktop-vm", tools: [machineEntry()],
        machines: [{ id: "desktop-vm", name: "Desktop VM", workspace: "/app", capabilities: ["shell"] }],
      }));
      await expect(ready).resolves.toEqual({ type: "ready" });
      return socket;
    };
    const first = await attach();
    const provider = new AccountHostedToolsProvider(namespace, owner, () => true);
    await provider.refresh();
    expect(provider.machineOnline("desktop-vm")).toBe(true);
    const admittedContext = { sessionId: "agent", callId: "admitted-before-disconnect" };
    const admittedInput = { cmd: "touch receipt", workdir: "/app" };
    const admittedFrame = nextFrame(first);
    const admitted = provider.machineTool("desktop-vm", "exec_command")!.handler(admittedInput, admittedContext);
    first.send(JSON.stringify({
      type: "result", call_id: (await admittedFrame).call_id,
      outcome: { status: "completed", output: {
        output: "saved receipt", success: true,
        structured_result: { output: "saved receipt", exit_code: 0, wall_time_seconds: 0 },
        metadata: null, process_trace: null,
      } },
    }));
    await expect(admitted).resolves.toMatchObject({ output: "saved receipt" });
    first.close(1000, "VM stopped");
    await vi.waitFor(async () => {
      await provider.refresh();
      expect(provider.machineOnline("desktop-vm")).toBe(false);
    });
    // Retain the namespace so previously admitted calls can still resolve receipts.
    expect(provider.machines().map(({ id }) => id)).toEqual(["desktop-vm"]);
    await expect(provider.machineTool("desktop-vm", "exec_command")!.handler(admittedInput, admittedContext))
      .resolves.toMatchObject({ success: true, output: "saved receipt" });
    const tools = createNamespaceExecutionTools(
      () => provider.machines(),
      (id, name, context) => provider.machineTool(id, name, context),
    );
    const router = new ToolRouter([toolMapSource("namespace", tools)]);
    const context = (callId: string) => ({
      sessionId: "agent", callId, model: "fixture", signal: new AbortController().signal,
    });
    const unavailable = await router.execute("exec_command", {
      cmd: "command -v blender", workdir: "/desktop-vm",
    }, context("offline-probe"));
    expect(unavailable).toMatchObject({
      success: false,
      output: expect.stringContaining("did not start tool execution"),
      structuredResult: { status: "unavailable" },
    });

    const successor = await attach();
    try {
      await provider.refresh();
      expect(provider.machineOnline("desktop-vm")).toBe(true);
      const call = nextFrame(successor);
      const completed = router.execute("exec_command", {
        cmd: "command -v blender", workdir: "/desktop-vm",
      }, context("reconnected-probe"));
      const frame = await call;
      expect(frame).toMatchObject({ type: "call", name: "exec_command", input: { workdir: "/app" } });
      successor.send(JSON.stringify({
        type: "result", call_id: frame.call_id,
        outcome: { status: "completed", output: {
          output: "/usr/bin/blender", success: true,
          structured_result: { output: "/usr/bin/blender", exit_code: 0, wall_time_seconds: 0 },
          metadata: null, process_trace: null,
        } },
      }));
      await expect(completed).resolves.toMatchObject({ success: true, output: "/usr/bin/blender" });
    } finally {
      successor.close(1000, "test complete");
    }
  });

  it("returns a known unstarted call to the agent without requesting durable replay", async () => {
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
      if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
      return Response.json({
        output: "Hosted machine is reconnecting", structured_result: { status: "unavailable" },
        success: false, metadata: null, value: null, pre_admission_unavailable: true,
      });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    const result = await provider.machineTool("laptop", "exec_command")!.handler({}, {
      sessionId: "agent", callId: "unstarted",
    });
    expect(result).toMatchObject({ success: false, structuredResult: { status: "unavailable" } });
    expect((result as Record<PropertyKey, unknown>)[HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE]).toBe(true);
    expect((result as { output: string }).output).toContain("Hosted machine is reconnecting");
  });

  it.each(["transport", "truncated", "invalid", "stale", "missing", "server"])(
    "retains call identity for %s failures where prior admission is unknown", async (mode) => {
      const calls: Record<string, unknown>[] = [];
      const transportError = new Error("connection lost");
      const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
        if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
        calls.push(await request.json<Record<string, unknown>>());
        if (calls.length > 1) return Response.json({
          output: "retained receipt", structured_result: null, success: true, metadata: null, value: "retained receipt",
        });
        if (mode === "transport") throw transportError;
        if (mode === "truncated") return new Response("{");
        if (mode === "invalid") return Response.json({ success: true });
        return new Response(null, { status: mode === "stale" ? 409 : mode === "missing" ? 404 : 503 });
      }]])), ACCOUNT_A, () => true);
      await provider.refresh();
      const context = { sessionId: "agent", callId: "possibly-admitted" };
      const tool = provider.machineTool("laptop", "exec_command")!;
      const failure = tool.handler({ cmd: "touch receipt" }, context);
      await expect(failure).rejects.toMatchObject({ code: "host_interrupted" });
      if (mode === "truncated" || mode === "invalid") {
        await expect(failure).rejects.toThrow("response could not be decoded");
      } else if (mode === "transport") {
        await expect(failure).rejects.toMatchObject({ cause: transportError });
      }
      await expect(tool.handler({ cmd: "touch receipt" }, context)).resolves.toMatchObject({ output: "retained receipt" });
      expect(calls).toHaveLength(2);
      expect(calls[1]).toEqual(calls[0]);
    },
  );

  it("releases stalled discovery and fences its late response from the next refresh", async () => {
    vi.useFakeTimers();
    try {
      const stalled = Promise.withResolvers<Response>();
      let attempts = 0;
      const provider = new AccountHostedToolsProvider({
        getByName: () => ({
          fetch: () => ++attempts === 1 ? stalled.promise : Promise.resolve(Response.json(snapshot)),
        }),
      } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => true);

      const initial = expect(provider.refresh()).rejects.toMatchObject({ code: "host_interrupted" });
      await vi.advanceTimersByTimeAsync(10_000);
      await initial;
      expect(provider.machines()).toEqual([]);
      await provider.refresh();
      expect(provider.machines()).toEqual(snapshot.machines.map(({ machine }) => machine));

      stalled.resolve(Response.json({ tools: [], machines: [] }));
      await vi.advanceTimersByTimeAsync(0);
      expect(attempts).toBe(2);
      expect(provider.machines()).toEqual(snapshot.machines.map(({ machine }) => machine));
    } finally {
      vi.useRealTimers();
    }
  });

  it("routes ten simultaneous account hands by machine identity", async () => {
    const namespace = (env as unknown as {
      NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
    }).NANOCODEX_ACCOUNT_TOOLS;
    const stub = namespace.getByName(ACCOUNT_A);
    const sockets = await Promise.all(Array.from({ length: 10 }, async (_, index) => {
      const id = `hand-${index}`;
      const upgraded = await stub.fetch("https://account-tools.internal/tool-host", {
        headers: { upgrade: "websocket", "x-nanocodex-owner-id": ACCOUNT_A },
      });
      expect(upgraded.status).toBe(101);
      const socket = upgraded.webSocket!;
      socket.accept();
      const ready = nextFrame(socket);
      socket.send(JSON.stringify({
        type: "catalog",
        attachment_id: id,
        tools: [machineEntry()],
        machines: [{
          id,
          name: `Hand ${index}`,
          workspace: `/workspace/${index}`,
          capabilities: ["shell", "vm"],
        }],
      }));
      await expect(ready).resolves.toEqual({ type: "ready" });
      return socket;
    }));

    const provider = new AccountHostedToolsProvider(namespace, ACCOUNT_A, () => true);
    await provider.refresh();
    expect(provider.machines().map(({ id }) => id)).toEqual(
      Array.from({ length: 10 }, (_, index) => `hand-${index}`),
    );

    const outputs = await Promise.all(sockets.map(async (socket, index) => {
      const id = `hand-${index}`;
      const call = nextFrame(socket);
      const result = provider.machineTool(id, "exec_command")!.handler(
        { cmd: "pwd", workdir: `/workspace/${index}` },
        { sessionId: `agent-${index % 2}`, callId: `call-${index}` },
      );
      const frame = await call;
      expect(frame).toMatchObject({ type: "call", name: "exec_command" });
      socket.send(JSON.stringify({
        type: "result",
        call_id: frame.call_id,
        outcome: {
          status: "completed",
          output: {
            output: `from ${id}`,
            success: true,
            structured_result: { exit_code: 0 },
            metadata: null,
            process_trace: null,
          },
        },
      }));
      return result;
    }));
    expect(outputs.map((output) => (output as { output: string }).output)).toEqual(
      Array.from({ length: 10 }, (_, index) => `from hand-${index}`),
    );
    for (const socket of sockets) socket.close(1000, "test complete");
  });

  it("keeps one live durable socket routable for calls from two agents", async () => {
    const namespace = (env as unknown as {
      NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
    }).NANOCODEX_ACCOUNT_TOOLS;
    const stub = namespace.getByName(crypto.randomUUID());
    const upgraded = await stub.fetch("https://account-tools.internal/tool-host", {
      headers: {
        upgrade: "websocket",
        "x-nanocodex-owner-id": ACCOUNT_A,
      },
    });
    expect(upgraded.status).toBe(101);
    const socket = upgraded.webSocket!;
    socket.accept();
    const ready = nextFrame(socket);
    socket.send(JSON.stringify({
      type: "catalog",
      tools: snapshot.tools.map(({ definition, route_token: _routeToken, ...entry }) => ({
        ...entry,
        definition: { ...definition, defer_loading: undefined },
      })),
      attachment_id: "fixture",
    }));
    await expect(ready).resolves.toEqual({ type: "ready" });

    const durableSnapshot = await stub.fetch("https://account-tools.internal/snapshot", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ owner_id: ACCOUNT_A }),
    });
    const durableBody = await durableSnapshot.json<typeof snapshot>();
    expect(durableBody).toMatchObject({
      tools: [{ definition: { name: "fixture__lookup" } }],
      machines: [],
    });
    expect(typeof durableBody.tools[0]!.route_token).toBe("string");
    const forbidden = await stub.fetch("https://account-tools.internal/snapshot", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ owner_id: ACCOUNT_B }),
    });
    expect(forbidden.status).toBe(404);

    for (const [sessionId, callId] of [["agent-a", "call-a"], ["agent-b", "call-b"]]) {
      const call = nextFrame(socket);
      const invoked = stub.fetch("https://account-tools.internal/invoke", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          owner_id: ACCOUNT_A,
          name: "fixture__lookup",
          input: {},
          session_id: sessionId,
          call_id: callId,
          model: "fixture-model",
          route_token: durableBody.tools[0]!.route_token,
        }),
      });
      const frame = await call;
      expect(frame).toMatchObject({ type: "call", session_id: sessionId });
      socket.send(JSON.stringify({
        type: "result",
        call_id: frame.call_id,
        outcome: {
          status: "completed",
          output: {
            output: `ran for ${sessionId}`,
            success: true,
            structured_result: { session_id: sessionId },
            metadata: null,
            process_trace: null,
          },
        },
      }));
      await expect((await invoked).json()).resolves.toMatchObject({
        success: true,
        value: { session_id: sessionId },
      });
    }
    const replacement = await stub.fetch("https://account-tools.internal/tool-host", {
      headers: { upgrade: "websocket", "x-nanocodex-owner-id": ACCOUNT_A },
    });
    const successor = replacement.webSocket!;
    successor.accept();
    const successorReady = nextFrame(successor);
    successor.send(JSON.stringify({
      type: "catalog",
      tools: snapshot.tools.map(({ definition, route_token: _routeToken, ...entry }) => ({
        ...entry,
        definition: { ...definition, defer_loading: undefined },
      })),
      attachment_id: "fixture",
    }));
    await successorReady;
    const stale = await stub.fetch("https://account-tools.internal/invoke", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        owner_id: ACCOUNT_A,
        name: "fixture__lookup",
        input: {},
        session_id: "agent-a",
        call_id: "stale-call",
        model: "fixture-model",
        route_token: durableBody.tools[0]!.route_token,
      }),
    });
    expect(stale.status).toBe(409);
    successor.close(1000, "test complete");
  });

  it("shares one account hand across independent agent session IDs", async () => {
    const calls: Record<string, unknown>[] = [];
    let snapshotLoads = 0;
    const namespace = fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
      const url = new URL(request.url);
      if (url.pathname === "/snapshot") {
        snapshotLoads += 1;
        return Response.json(snapshot);
      }
      const body = await request.json<Record<string, unknown>>();
      calls.push(body);
      return Response.json({
        output: `ran for ${body.session_id}`,
        structured_result: { session_id: body.session_id },
        success: true,
        metadata: null,
        value: body.session_id,
      });
    }]]));
    const provider = new AccountHostedToolsProvider(namespace, ACCOUNT_A, () => true);
    provider.setCatalogValidator((candidates) => {
      expect(candidates[0]).toMatchObject({ provider: "fixture", remote_name: "lookup" });
      return true;
    });
    await provider.refresh();
    await provider.settled();

    expect(snapshotLoads).toBe(1);
    expect(provider.definitions()).toEqual([snapshot.tools[0]!.definition]);
    expect(provider.machines()).toEqual(snapshot.machines.map(({ machine }) => machine));
    const tool = provider.resolve("fixture__lookup")!;
    const [left, right] = await Promise.all([
      tool.handler({}, {
        sessionId: "agent-a",
        callId: "call-a",
      }),
      tool.handler({}, {
        sessionId: "agent-b",
        callId: "call-b",
      }),
    ]);

    expect((left as Record<PropertyKey, unknown>)[TOOL_RESULT]).toBe(true);
    expect((right as Record<string, unknown>).value).toBe("agent-b");
    expect(calls.map((call) => call.session_id)).toEqual(["agent-a", "agent-b"]);
  });

  it("uses account-keyed objects and hides the catalog outside account-owned turns", async () => {
    let allowed = true;
    const requested: string[] = [];
    const namespace = fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
      requested.push(ACCOUNT_A);
      return new URL(request.url).pathname === "/snapshot"
        ? Response.json(snapshot)
        : new Response(null, { status: 404 });
    }]]), requested);
    const owned = new AccountHostedToolsProvider(namespace, ACCOUNT_A, () => allowed);
    const other = new AccountHostedToolsProvider(namespace, ACCOUNT_B, () => true);
    await Promise.all([owned.refresh(), other.refresh()]);

    expect(owned.definitions()).toHaveLength(1);
    expect(other.definitions()).toEqual([]);
    expect(owned.machineOnline("laptop")).toBe(true);
    expect(other.machineOnline("laptop")).toBe(false);
    allowed = false;
    expect(owned.definitions()).toEqual([]);
    expect(owned.machines()).toEqual([]);
    expect(owned.machineOnline("laptop")).toBe(false);
    expect(requested).toContain(ACCOUNT_B);
  });

  it("rechecks account hand calls against the invoking subagent context", async () => {
    const invoked: string[] = [];
    const namespace = fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
      if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
      const body = await request.json<{ session_id: string }>();
      invoked.push(body.session_id);
      return Response.json({
        output: "ok",
        structured_result: null,
        success: true,
        metadata: null,
        value: "ok",
      });
    }]]));
    const provider = new AccountHostedToolsProvider(
      namespace,
      ACCOUNT_A,
      (context) => context === undefined || context.sessionId === "allowed-child",
    );
    await provider.refresh();
    const tool = provider.resolve("fixture__lookup")!;

    const denied = await tool.handler({}, {
      sessionId: "denied-child",
      callId: "call-denied",
    });
    expect(denied).toMatchObject({
      success: false,
      structuredResult: { status: "unavailable" },
    });
    expect(invoked).toEqual([]);

    await tool.handler({}, { sessionId: "allowed-child", callId: "call-allowed" });
    expect(invoked).toEqual(["allowed-child"]);
  });

  it("fails closed for malformed snapshots and duplicate public tool names", async () => {
    const malformed = [
      null,
      {
        tools: [snapshot.tools[0], snapshot.tools[0], snapshot.tools[0]],
        machines: [],
      },
    ];
    for (const body of malformed) {
      const namespace = fakeNamespace(new Map([[ACCOUNT_A, async () => Response.json(body)]]));
      const provider = new AccountHostedToolsProvider(namespace, ACCOUNT_A, () => true);
      await expect(provider.refresh()).resolves.toBeUndefined();
      expect(provider.definitions()).toEqual([]);
      expect(provider.machines()).toEqual([]);
    }
  });

  it("pins invocation to the discovered catalog and brands truncated results ambiguous", async () => {
    let mode: "stale" | "truncated" = "stale";
    const namespace = fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
      if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
      if (mode === "stale") return Response.json({ error: "stale_catalog" }, { status: 409 });
      return new Response("{", { headers: { "content-type": "application/json" } });
    }]]));
    const provider = new AccountHostedToolsProvider(namespace, ACCOUNT_A, () => true);
    await provider.refresh();
    const tool = provider.resolve("fixture__lookup")!;

    const stale = await tool.handler({}, { sessionId: "agent-a", callId: "call-stale" });
    expect(stale).toMatchObject({ success: false, structuredResult: { status: "unavailable" } });
    mode = "truncated";
    const truncated = await tool.handler({}, { sessionId: "agent-a", callId: "call-truncated" });
    expect(truncated).toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
  });
});

type Handler = (request: Request) => Promise<Response>;

function fakeNamespace(
  handlers: Map<string, Handler>,
  requested: string[] = [],
): DurableObjectNamespace<AccountHostedTools> {
  return {
    getByName(name: string) {
      requested.push(name);
      const handler = handlers.get(name);
      return {
        fetch(input: RequestInfo | URL, init?: RequestInit) {
          return handler?.(new Request(input, init))
            ?? Promise.resolve(new Response(null, { status: 404 }));
        },
      };
    },
  } as unknown as DurableObjectNamespace<AccountHostedTools>;
}

function nextFrame(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const onMessage = (event: MessageEvent) => {
      cleanup();
      try { resolve(JSON.parse(String(event.data)) as Record<string, unknown>); }
      catch (error) { reject(error); }
    };
    const onError = () => {
      cleanup();
      reject(new Error("account Hosted Tools socket failed"));
    };
    const cleanup = () => {
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("error", onError);
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
  });
}

function machineEntry() {
  return {
    provider: "machine",
    remote_name: "exec_command",
    definition: {
      type: "function" as const,
      name: "exec_command",
      description: "Canonical machine exec_command",
      strict: false,
      parameters: EXEC_COMMAND_PARAMETERS,
      output_schema: EXECUTION_OUTPUT_SCHEMA,
    },
    parallel_safe: true,
    summary: "Machine exec_command",
    timeout_ms: 30_000,
  };
}
