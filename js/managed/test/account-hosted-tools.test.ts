import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
} from "nanocodex-tools/execution-contract";
import { HOSTED_TOOLS_PRE_ADMISSION_UNAVAILABLE } from "nanocodex-tools/hosted";
// @ts-expect-error The runtime subpath is intentionally JavaScript-only.
import { ToolRouter, toolMapSource } from "nanocodex-tools/runtime/tool-router";
import { SqlHostedToolsPersistence } from "../src/hosted-tools-broker";
import { createNamespaceExecutionTools } from "../src/namespace-tools";
import { screenTool } from "../src/hand-remote-agent";

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
  it("retains a machine provider declaration across account discovery", async () => {
    const definition = { type: "function" as const, name: "mcp__cua_repl__js",
      description: "Provider startup: await desktop.connect()", strict: false,
      parameters: { type: "object", properties: { code: { type: "string" } }, required: ["code"] },
      defer_loading: true as const };
    const catalog = { tools: [], machines: [{ ...snapshot.machines[0], tools: [{
      name: "mcp__cua_repl__js", parallel_safe: true, route_token: "cua-route", definition,
    }] }] };
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([
      [ACCOUNT_A, async () => Response.json(catalog)],
    ])), ACCOUNT_A, () => true);
    await provider.refresh();
    expect(provider.machineTool("laptop", "mcp__cua_repl__js")?.definition).toEqual(definition);
  });
  it("joins screen discovery by machine identity without promoting an offline factory", async () => {
    const target = { machine_id: "laptop", machine_name: "Build laptop", id: "desktop", name: "Desktop",
      kind: "desktop", generation: "screen-generation", width: 1280, height: 800, controllable: true, agent_tools: true };
    let catalog = { ...snapshot, screens: [target], tools: [...snapshot.tools, screenTool(target)],
      machines: [{ ...snapshot.machines[0]!, online: false }] };
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async () => Response.json(catalog)]])), ACCOUNT_A, () => true);
    await provider.refresh();
    expect(provider.machines()).toHaveLength(1);
    expect(provider.machines()[0]).toMatchObject({ id: "laptop", workspace: "/work/nanocodex",
      capabilities: ["filesystem", "native-shell", "computer", "screen"] });
    expect(provider.machineOnline("laptop")).toBe(false);
    expect(provider.screenTool("laptop")).toBeDefined();
    expect(provider.definitions().map(tool => tool.name)).not.toContain(screenTool(target).definition.name);
    expect(provider.resolve(screenTool(target).definition.name)).toBeUndefined();
    expect(provider.screenTool("other")).toBeUndefined();
    // Metadata alone cannot bind a route for a different screen generation.
    catalog = { ...catalog, screens: [{ ...target, generation: "replacement" }] };
    await provider.refresh();
    expect(provider.screenTool("laptop")).toBeUndefined();
    expect(provider.screenMachines()).toEqual([]);
    expect(provider.machines()[0]!.capabilities).not.toContain("screen");
  });
  it.each(["mac", "windows", "linux", "phone"])("keeps %s screen publishers internal to the viewer", async kind => {
    const target = { machine_id: `screen-${kind}`, machine_name: "Fixture screen", id: "desktop", name: "Screen",
      kind, generation: "generation", width: 800, height: 600, controllable: true, agent_tools: true };
    const published = screenTool(target);
    const catalog = { tools: [published], machines: [], screens: [target] };
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([
      [ACCOUNT_A, async () => Response.json(catalog)],
    ])), ACCOUNT_A, () => true);
    await provider.refresh();
    expect(provider.screenTool(target.machine_id)).toBeDefined();
    expect(provider.definitions()).toEqual([]);
    expect(provider.resolve(published.definition.name)).toBeUndefined();
  });
  it("returns transitioned SQL rows without a second SELECT and retains failed transitions", async () => {
    const namespace = (env as unknown as {
      NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
    }).NANOCODEX_ACCOUNT_TOOLS;
    await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, context) => {
      const persistence = new SqlHostedToolsPersistence(context.storage);
      persistence.initialize(Date.now());
      const row = { call_id: "transport", session_id: "agent", source_call_id: "source", host_id: "host",
        lease_id: "lease", generation: 1, model: "fixture", name: "exec_command", input_json: "{}",
        output_token_budget: 100, output_byte_budget: 1024, deadline_at: Date.now() + 60_000,
        cancel_requested: 0, state: "admitted" as const, result_json: null, receipt_json: null };
      persistence.insertCall(row, Date.now());
      const read = vi.spyOn(persistence, "call");
      expect(persistence.transitionCall("transport", ["admitted"], "dispatched", "", Date.now()))
        .toEqual({ ...row, state: "dispatched" });
      const result = JSON.stringify({ status: "completed", output: "receipt" });
      expect(persistence.transitionCall("transport", ["dispatched"], "completed", result, Date.now()))
        .toEqual({ ...row, state: "completed", result_json: result });
      expect(read).not.toHaveBeenCalled();
      expect(persistence.transitionCall("transport", ["dispatched"], "ambiguous", "conflict", Date.now()))
        .toEqual({ ...row, state: "completed", result_json: result });
      expect(read).toHaveBeenCalledExactlyOnceWith("transport");
      expect(persistence.transitionCall("missing", ["dispatched"], "cancelled", "", Date.now())).toBeUndefined();
    });
  });

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
      expect(calls).toHaveLength(1);
      await expect(tool.handler({ cmd: "touch receipt" }, context)).resolves.toMatchObject({ output: "retained receipt" });
      expect(calls).toHaveLength(2);
      expect(calls[1]).toEqual(calls[0]);
    },
  );

  it("bounds stale-route recovery even when the replacement route is rejected", async () => {
    const calls: Record<string, unknown>[] = [];
    let discoveries = 0;
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
      if (new URL(request.url).pathname === "/snapshot") {
        discoveries++;
        return Response.json({ ...snapshot, machines: [{ ...snapshot.machines[0], tools: [{
          ...snapshot.machines[0]!.tools[0], route_token: `route-${discoveries}`,
        }] }] });
      }
      calls.push(await request.json<Record<string, unknown>>());
      return new Response(null, { status: 409 });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    await expect(provider.machineTool("laptop", "exec_command")!.handler(
      { cmd: "fixture-effect" }, { sessionId: "agent", callId: "stable-effect" },
    )).rejects.toMatchObject({ code: "host_interrupted" });
    expect(discoveries).toBe(2);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual({ ...calls[0], route_token: "route-2" });
  });

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

it("reuses bounded discovery but keeps forced refresh and live authority", async () => {
  let now = 1000, allowed = true;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  const fetch = vi.fn(async () => Response.json(snapshot));
  const provider = new AccountHostedToolsProvider({ getByName: () => ({ fetch }) } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => allowed);
  try {
    await Promise.all([provider.refresh(120_000), provider.refresh(120_000)]);
    await provider.refresh(120_000);
    expect(fetch).toHaveBeenCalledTimes(1);
    allowed = false;
    expect(provider.definitions()).toEqual([]);
    expect(provider.machineTool("laptop", "exec_command")).toBeUndefined();
    allowed = true;
    expect(provider.definitions()).not.toEqual([]);
    now += 120_000;
    await provider.refresh(120_000);
    expect(fetch).toHaveBeenCalledTimes(2);
    await provider.refresh();
    expect(fetch).toHaveBeenCalledTimes(3);
    provider.invalidate();
    fetch.mockImplementationOnce(async () => new Response(null, { status: 503 }));
    await expect(provider.refresh(120_000)).rejects.toThrow();
    await provider.refresh(120_000);
    expect(fetch).toHaveBeenCalledTimes(5);
  } finally { clock.mockRestore(); }
});

it("an invalidated in-flight discovery cannot publish or satisfy the next refresh", async () => {
  let release!: (response: Response) => void;
  const first = new Promise<Response>(resolve => { release = resolve; });
  const fetch = vi.fn().mockImplementationOnce(() => first)
    .mockImplementation(async () => Response.json({ tools: [], machines: [] }));
  const provider = new AccountHostedToolsProvider({ getByName: () => ({ fetch }) } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => true);
  const pending = provider.refresh(120_000);
  provider.invalidate();
  const replacement = provider.refresh(120_000);
  release(Response.json(snapshot));
  await Promise.all([pending, replacement]);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(provider.definitions()).toEqual([]);
  await provider.refresh(120_000);
  expect(fetch).toHaveBeenCalledTimes(2);
});
