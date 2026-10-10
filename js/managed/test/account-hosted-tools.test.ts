import { env } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  WRITE_STDIN_PARAMETERS,
} from "nanocodex-tools/execution-contract";
// @ts-expect-error The runtime subpath is intentionally JavaScript-only.
import { ToolRouter, toolMapSource } from "nanocodex-tools/runtime/tool-router";
import { createNamespaceExecutionTools } from "../src/namespace-tools";
import { screenTool } from "../src/hand-remote-agent";

import {
  AccountHostedTools,
  AccountHostedToolsProvider,
} from "../src/account-hosted-tools";

const ACCOUNT_A = "11111111-1111-4111-8111-111111111111";
const ACCOUNT_B = "22222222-2222-4222-8222-222222222222";

const hostSockets = new Map<WebSocket, Promise<void>>();

function acceptHostSocket(socket: WebSocket) {
  const closed = new Promise<void>(resolve => {
    socket.addEventListener("close", event => {
      // A replaced host receives a server-initiated close and must acknowledge it.
      socket.close(event.code === 1005 ? 1000 : event.code, event.reason);
      resolve();
    }, { once: true });
  });
  hostSockets.set(socket, closed);
  socket.accept();
}

async function closeHostSocket(socket: WebSocket, reason = "test complete") {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    socket.close(1000, reason);
    await Promise.race([
      hostSockets.get(socket),
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`account Hosted Tools socket close handshake timed out (readyState ${socket.readyState})`)), 3_000);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

afterEach(async () => {
  try {
    await Promise.all([...hostSockets.keys()].map(socket => closeHostSocket(socket)));
  } finally {
    hostSockets.clear();
  }
});

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
  // A pending approval belongs to one helper route; discovery must not retarget it.
  it("never refreshes a rejected native secure input route", async () => {
    let discoveries=0, invocations=0;
    const catalog={tools:[],machines:[{...snapshot.machines[0],tools:[{name:"native_secure_input",parallel_safe:false,route_token:"native-route"}]}]};
    const provider=new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A,async (request:Request)=>{
      if(new URL(request.url).pathname==="/invoke"){invocations++;return new Response(null,{status:409});}
      discoveries++;return Response.json(catalog);
    }]])),ACCOUNT_A,()=>true);
    await provider.refresh();
    const tool=provider.machineTool("laptop","native_secure_input");
    expect(tool).toBeDefined();
    await tool!.handler({operation:"submit",request_id:crypto.randomUUID(),ciphertext:"encrypted"},{sessionId:"agent",callId:"private-submit"});
    expect(invocations).toBe(1);
    expect(discoveries).toBe(1);
  });

  it("exposes only the current generation's in-flight inventory for admission to join", async () => {
    let release!: () => void;
    const held = new Promise<void>(resolve => { release = resolve; });
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async () => {
      await held;
      return Response.json(snapshot);
    }]])), ACCOUNT_A, () => true);
    expect(provider.pendingRefresh()).toBeUndefined();
    const refreshing = provider.refresh();
    const pending = provider.pendingRefresh();
    expect(pending).toBeDefined();
    expect(provider.machines()).toEqual([]);
    release();
    await pending;
    await refreshing;
    expect(provider.machines().map(machine => machine.id)).toEqual(["laptop"]);
    expect(provider.pendingRefresh()).toBeUndefined();
    // An authorization change fences the old request: admission must not join it.
    let releaseStale!: () => void;
    const stale = new Promise<void>(resolve => { releaseStale = resolve; });
    const fenced = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async () => {
      await stale;
      return Response.json(snapshot);
    }]])), ACCOUNT_A, () => true);
    const old = fenced.refresh();
    expect(fenced.pendingRefresh()).toBeDefined();
    fenced.invalidate();
    expect(fenced.pendingRefresh()).toBeUndefined();
    releaseStale();
    await old;
    expect(fenced.machines()).toEqual([]);
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
      acceptHostSocket(socket);
      const ready = nextFrame(socket);
      socket.send(JSON.stringify({
        type: "catalog", capabilities: ["turn_metadata"], attachment_id: "desktop-vm", tools: [machineEntry()],
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
    await closeHostSocket(first, "VM stopped");
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
      await closeHostSocket(successor);
    }
  });

  // Lost or undecodable responses are reconciled receipt-only; see the
  // "Hand receipt reconciliation" journeys. HTTP rejections stay pinned.
  it.each(["stale", "missing", "server"])(
    "retains call identity for %s failures where prior admission is unknown", async (mode) => {
      const calls: Record<string, unknown>[] = [];
      const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async (request) => {
        if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
        calls.push(await request.json<Record<string, unknown>>());
        if (calls.length > 1) return Response.json({
          output: "retained receipt", structured_result: null, success: true, metadata: null, value: "retained receipt",
        });
        return new Response(null, { status: mode === "stale" ? 409 : mode === "missing" ? 404 : 503 });
      }]])), ACCOUNT_A, () => true);
      await provider.refresh();
      const context = { sessionId: "agent", callId: "possibly-admitted" };
      const tool = provider.machineTool("laptop", "exec_command")!;
      const failure = tool.handler({ cmd: "touch receipt" }, context);
      await expect(failure).resolves.toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
      expect(calls).toHaveLength(1);
      await expect(tool.handler({ cmd: "touch receipt" }, context)).resolves.toMatchObject({ output: "retained receipt" });
      expect(calls).toHaveLength(2);
      expect(calls[1]).toEqual(calls[0]);
    },
  );

  it.each(["ambiguous", "unavailable", "provider_error"])("observes decoded %s as its structured outcome", async status => {
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
      if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
      return Response.json({ output: "fixture error", structured_result: { status }, success: false,
        metadata: null, value: null });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    const logs: Record<string, unknown>[] = [];
    const spy = vi.spyOn(console, "info").mockImplementation(entry => { logs.push(entry); });
    try {
      await expect(provider.machineTool("laptop", "exec_command")!.handler({}, {
        sessionId: "agent", callId: "fixture-call",
      })).resolves.toMatchObject({ success: false, structuredResult: { status } });
      expect(logs).toContainEqual(expect.objectContaining({ type: "hand.tool.stage", stage: "account.decode",
        outcome: status === "provider_error" ? "failed" : status, call_id: "fixture-call" }));
    } finally { spy.mockRestore(); }
  });

  it("settles a broken Hand locally while another tool in the same agent continues", async () => {
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
      if (new URL(request.url).pathname === "/snapshot") return Response.json(snapshot);
      throw new Error("fixture network disconnected");
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    const broken = provider.machineTool("laptop", "exec_command")!;
    const router = new ToolRouter([toolMapSource("fixture", {
      broken: { description: "Broken Hand", parameters: { type: "object" },
        handler: (input: unknown, context: Parameters<typeof broken.handler>[1]) => broken.handler(input, context), supportsParallelToolCalls: true },
      healthy: { description: "Independent work", parameters: { type: "object" },
        handler: () => "still running", supportsParallelToolCalls: true },
    })]);
    const controller = new AbortController();
    const context = { sessionId: "agent", model: "fixture", signal: controller.signal };
    const [failed, healthy] = await Promise.all([
      router.execute("broken", {}, { ...context, callId: "broken" }),
      router.execute("healthy", {}, { ...context, callId: "healthy" }),
    ]);
    expect(failed).toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
    expect(healthy).toBe("still running");
    expect(controller.signal.aborted).toBe(false);
  });

  it("contains a failed routing refresh without an invocation resend", async () => {
    let reads = 0;
    let sends = 0;
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
      if (new URL(request.url).pathname === "/snapshot") {
        if (++reads > 1) throw new Error("discovery unavailable");
        return Response.json(snapshot);
      }
      sends++;
      return new Response(null, { status: 409 });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    await expect(provider.machineTool("laptop", "exec_command")!.handler({}, {
      sessionId: "agent", callId: "one-call",
    })).resolves.toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
    expect(sends).toBe(1);
  });

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
      // The account shard's ledger proves each attempt was never admitted.
      return Response.json({ error: "stale_catalog", admission: "none" }, { status: 409 });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    await expect(provider.machineTool("laptop", "exec_command")!.handler(
      { cmd: "fixture-effect" }, { sessionId: "agent", callId: "stable-effect" },
    )).resolves.toMatchObject({ success: false, structuredResult: { status: "unavailable", admitted: false, resent: false } });
    expect(discoveries).toBe(2);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual({ ...calls[0], route_token: "route-2" });
  });

  it("recovers a cached iPhone contact tool through the real broker after socket replacement", async () => {
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
      acceptHostSocket(socket);
      const ready = nextFrame(socket);
      socket.send(JSON.stringify({
        type: "catalog", capabilities: ["turn_metadata"], attachment_id: "fixture-phone",
        machines: [{ id: "fixture-phone", name: "Fixture iPhone", workspace: "/app", capabilities: ["contacts"] }],
        tools: [{
          provider: "machine", remote_name: "search_contacts", parallel_safe: true, timeout_ms: 10_000,
          definition: { type: "function", name: "search_contacts", description: "Search fixture contacts", strict: false,
            parameters: { type: "object", properties: { query: { type: "string" } }, additionalProperties: false } },
        }],
      }));
      await expect(ready).resolves.toEqual({ type: "ready" });
      return socket;
    };
    const first = await attach();
    const provider = new AccountHostedToolsProvider(namespace, owner, () => true);
    await provider.refresh();
    const cached = provider.resolve("user_fixture-phone_search_contacts")!;
    expect(cached).toBeDefined();
    const successor = await attach();
    try {
      const framePromise = nextFrame(successor);
      const completed = cached.handler({ query: "Example" }, { sessionId: "agent", callId: "contact-lookup" });
      const frame = await framePromise;
      expect(frame).toMatchObject({ type: "call", name: "search_contacts", input: { query: "Example" } });
      successor.send(JSON.stringify({
        type: "result", call_id: frame.call_id,
        outcome: { status: "completed", output: { output: "contact found", success: true,
          structured_result: { contacts: [] }, metadata: null, process_trace: null } },
      }));
      await expect(completed).resolves.toMatchObject({ success: true, output: "contact found" });
      expect(provider.resolve("user_fixture-phone_search_contacts")!.routeToken).not.toBe(cached.routeToken);
    } finally {
      await closeHostSocket(first);
      await closeHostSocket(successor);
    }
  });

  it.each([404, 409])("refreshes a reconnected personal tool after a %s routing rejection", async status => {
    const calls: Record<string, unknown>[] = [];
    let discoveries = 0;
    const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
      if (new URL(request.url).pathname === "/snapshot") {
        discoveries++;
        return Response.json({ ...snapshot, tools: [{ ...snapshot.tools[0], route_token: `personal-${discoveries}` }] });
      }
      calls.push(await request.json<Record<string, unknown>>());
      if (calls.length === 1) return Response.json({ admission: "none" }, { status });
      return Response.json({ output: "contact found", structured_result: null, success: true, metadata: null, value: null });
    }]])), ACCOUNT_A, () => true);
    await provider.refresh();
    await expect(provider.resolve("fixture__lookup")!.handler({ query: "Example" }, {
      sessionId: "agent", turnId: "turn", callId: "contact-lookup",
    })).resolves.toMatchObject({ success: true, output: "contact found" });
    expect(discoveries).toBe(2);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual({ ...calls[0], route_token: "personal-2" });
  });

  it.each(["unchanged", "removed", "rejected", "revoked", "server", "transport", "truncated"])(
    "bounds personal tool recovery for %s routes and uncertain outcomes", async mode => {
      let discoveries = 0;
      let allowed = true;
      const calls: Record<string, unknown>[] = [];
      const provider = new AccountHostedToolsProvider(fakeNamespace(new Map([[ACCOUNT_A, async request => {
        if (new URL(request.url).pathname === "/snapshot") {
          discoveries++;
          if (discoveries > 1 && mode === "revoked") allowed = false;
          return Response.json({ ...snapshot, tools: mode === "removed" && discoveries > 1 ? [] : [{
            ...snapshot.tools[0], route_token: mode === "unchanged" ? "personal-1" : `personal-${discoveries}`,
          }] });
        }
        // /invoke-receipt reads are not executions; only /invoke can dispatch.
        if (new URL(request.url).pathname === "/invoke") calls.push(await request.json<Record<string, unknown>>());
        if (mode === "transport") throw new Error("connection lost");
        if (mode === "truncated") return new Response("{");
        return mode === "server" ? new Response(null, { status: 503 })
          : Response.json({ error: "stale_catalog", admission: "none" }, { status: 409 });
      }]])), ACCOUNT_A, () => allowed);
      await provider.refresh();
      const result = await provider.resolve("fixture__lookup")!.handler({}, { sessionId: "agent", callId: "lookup" });
      expect(result).toMatchObject({ success: false });
      expect(discoveries).toBe(["server", "transport", "truncated"].includes(mode) ? 1 : 2);
      expect(calls).toHaveLength(mode === "rejected" ? 2 : 1);
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
      acceptHostSocket(socket);
      const ready = nextFrame(socket);
      socket.send(JSON.stringify({
        type: "catalog", capabilities: ["turn_metadata"],
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
    await Promise.all(sockets.map(socket => closeHostSocket(socket)));
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
    acceptHostSocket(socket);
    const ready = nextFrame(socket);
    socket.send(JSON.stringify({
      type: "catalog", capabilities: ["turn_metadata"],
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
          turn_id: `${sessionId}:7`,
          call_id: callId,
          model: "fixture-model",
          route_token: durableBody.tools[0]!.route_token,
        }),
      });
      const frame = await call;
      expect(frame).toMatchObject({ type: "call", session_id: sessionId, turn_id: `${sessionId}:7` });
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
    acceptHostSocket(successor);
    const successorReady = nextFrame(successor);
    successor.send(JSON.stringify({
      type: "catalog", capabilities: ["turn_metadata"],
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
    await closeHostSocket(successor);
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
    const onClose = (event: CloseEvent) => {
      cleanup();
      reject(new Error(`account Hosted Tools socket closed before a frame (${event.code}: ${event.reason})`));
    };
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error("account Hosted Tools socket did not deliver a frame within 3000ms"));
    }, 3_000);
    const cleanup = () => {
      clearTimeout(timeout);
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("error", onError);
      socket.removeEventListener("close", onClose);
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
    socket.addEventListener("close", onClose);
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

it("settles optional account tools without waiting for cold discovery", async () => {
  const stalled = Promise.withResolvers<Response>();
  const fetch = vi.fn(() => stalled.promise);
  const provider = new AccountHostedToolsProvider({ getByName: () => ({ fetch }) } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => true);
  const refresh = provider.refreshOptional(120_000);
  await provider.settled();
  expect(provider.machines()).toEqual([]);
  stalled.resolve(Response.json(snapshot));
  await refresh;
  expect(provider.machines()).toHaveLength(1);
});

it("backs off optional failures while forced discovery and live authorization remain independent", async () => {
  let now = 1000, allowed = true;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  const fetch = vi.fn(async () => Response.json(snapshot));
  const provider = new AccountHostedToolsProvider({ getByName: () => ({ fetch }) } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => allowed);
  try {
    await provider.refreshOptional(120_000);
    const captured = provider.machineTool("laptop", "exec_command")!;
    now += 120_000;
    fetch.mockImplementation(async () => new Response(null, { status: 503 }));
    await expect(provider.refreshOptional(120_000)).rejects.toThrow("Account hand discovery interrupted");
    expect(provider.machines()).toHaveLength(1);
    await provider.refreshOptional(120_000);
    expect(fetch).toHaveBeenCalledTimes(2);
    allowed = false;
    expect(provider.definitions()).toEqual([]);
    expect(provider.machines()).toEqual([]);
    expect(provider.machineTool("laptop", "exec_command")).toBeUndefined();
    await captured.handler({ cmd: "must not run" }, { sessionId: "fixture", callId: "revoked" });
    expect(fetch).toHaveBeenCalledTimes(2);
    allowed = true;
    now += 10_000;
    await expect(provider.refreshOptional(120_000)).rejects.toThrow();
    expect(fetch).toHaveBeenCalledTimes(3);
    fetch.mockImplementation(async () => Response.json({ tools: [], machines: [] }));
    await provider.refresh();
    expect(fetch).toHaveBeenCalledTimes(4);
    expect(provider.machines()).toEqual([]);
  } finally { clock.mockRestore(); }
});

it("clears inventory on authority changes and fences a late prior discovery", async () => {
  const old = Promise.withResolvers<Response>();
  const fetch = vi.fn().mockResolvedValueOnce(Response.json(snapshot))
    .mockImplementationOnce(() => old.promise)
    .mockResolvedValue(Response.json({ tools: [], machines: [] }));
  const provider = new AccountHostedToolsProvider({ getByName: () => ({ fetch }) } as unknown as DurableObjectNamespace<AccountHostedTools>, ACCOUNT_A, () => true);
  await provider.refresh();
  const prior = provider.refresh();
  provider.invalidate({ clearCatalog: true });
  expect(provider.machines()).toEqual([]);
  expect(provider.definitions()).toEqual([]);
  expect(provider.machineTool("laptop", "exec_command")).toBeUndefined();
  const current = provider.refreshOptional(120_000);
  old.resolve(Response.json(snapshot));
  await prior;
  expect(provider.machines()).toEqual([]);
  await current;
  expect(fetch).toHaveBeenCalledTimes(3);
  expect(provider.machines()).toEqual([]);
});

describe("process session transport recovery", () => {
  async function fixture(runtimeId: string | undefined) {
    const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
    const owner = crypto.randomUUID();
    const stub = namespace.getByName(owner);
    const sockets: WebSocket[] = [];
    const attach = async (runtime: string | undefined) => {
      const response = await stub.fetch("https://account-tools.internal/tool-host", {
        headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
      });
      const socket = response.webSocket!;
      acceptHostSocket(socket); sockets.push(socket);
      const ready = nextFrame(socket);
      const writer = machineEntry();
      socket.send(JSON.stringify({ type: "catalog", capabilities: ["turn_metadata"],
        attachment_id: "session-hand", ...(runtime === undefined ? {} : { runtime_id: runtime }),
        machines: [{ id: "session-hand", name: "Session Hand", workspace: "/fixture", capabilities: ["process"] }],
        tools: [writer, { ...writer, remote_name: "write_stdin",
          definition: { ...writer.definition, name: "write_stdin", parameters: WRITE_STDIN_PARAMETERS } }],
      }));
      await expect(ready).resolves.toEqual({ type: "ready" });
      return socket;
    };
    const first = await attach(runtimeId);
    const provider = new AccountHostedToolsProvider(namespace, owner, () => true);
    await provider.refresh();
    const tools = createNamespaceExecutionTools(() => provider.machines(), (id, name, context) => provider.machineTool(id, name, context));
    const router = new ToolRouter([toolMapSource("namespace", tools)]);
    const context = (callId: string, sessionId = "agent") => ({ sessionId, callId, model: "fixture", signal: new AbortController().signal });
    const finish = async (socket: WebSocket, frame: Record<string, unknown>, result: Record<string, unknown>) => {
      const ack = nextFrame(socket);
      socket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: { status: "completed", output: {
        output: "process output", success: true, structured_result: { wall_time_seconds: 0, output: "process output", ...result },
        metadata: null, process_trace: null,
      } } }));
      await expect(ack).resolves.toEqual({ type: "ack", call_id: frame.call_id });
    };
    const start = async (socket = first) => {
      const frame = nextFrame(socket);
      const pending = router.execute("exec_command", { cmd: "fixture-command", workdir: "/session-hand" }, context("start"));
      await finish(socket, await frame, { session_id: 1 });
      return ((await pending) as { structuredResult: { session_id: number } }).structuredResult.session_id;
    };
    return { first, provider, attach, router, context, finish, start,
      close: () => Promise.all(sockets.map(socket => closeHostSocket(socket))) };
  }

  it("polls a retained process across reconnects, preserves ownership, and releases its completed binding", async () => {
    const f = await fixture("runtime-one");
    try {
      const session = await f.start();
      const oldWriter = f.provider.machineTool("session-hand", "write_stdin")!.routeToken;
      const oldExec = f.provider.machineTool("session-hand", "exec_command")!.routeToken;
      const second = await f.attach("runtime-one");
      await f.provider.refresh();
      expect(f.provider.machineTool("session-hand", "write_stdin")!.routeToken).toBe(oldWriter);
      expect(f.provider.machineTool("session-hand", "exec_command")!.routeToken).not.toBe(oldExec);
      await expect(f.router.execute("write_stdin", { session_id: session }, f.context("foreign", "other-agent"))).rejects.toThrow("unknown or stale");
      const frame = nextFrame(second);
      const poll = f.router.execute("write_stdin", { session_id: session }, f.context("poll"));
      const sent = await frame;
      expect(sent).toMatchObject({ name: "write_stdin", input: { session_id: 1 } });
      await f.finish(second, sent, { exit_code: 0 });
      await expect(poll).resolves.toMatchObject({ success: true, structuredResult: { exit_code: 0 } });
      await expect(f.router.execute("write_stdin", { session_id: session }, f.context("finished"))).rejects.toThrow("unknown or stale");
    } finally { await f.close(); }
  });

  it("does not replay an ambiguous poll or stdin when its socket is replaced", async () => {
    const f = await fixture("runtime-one");
    try {
      const session = await f.start();
      const frame = nextFrame(f.first);
      const input = { session_id: session, chars: "one write\n" };
      const poll = f.router.execute("write_stdin", input, f.context("pending-write"));
      await frame;
      const second = await f.attach("runtime-one");
      const sent: unknown[] = [];
      second.addEventListener("message", event => { sent.push(JSON.parse(String(event.data))); });
      await expect(poll).resolves.toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
      // Reconciliation keeps the same effect identity; the old receipt wins.
      await expect(f.router.execute("write_stdin", input, f.context("pending-write")))
        .resolves.toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
      expect(sent).toEqual([]);
      const next = nextFrame(second);
      const freshPoll = f.router.execute("write_stdin", { session_id: session }, f.context("next-poll"));
      await f.finish(second, await next, { exit_code: 0 });
      await expect(freshPoll).resolves.toMatchObject({ success: true });
      expect(sent.filter((frame: any) => frame.type === "call")).toHaveLength(1);
    } finally { await f.close(); }
  });

  it.each(["runtime-one", undefined])("fences saved polls and stdin after replacement of %s", async original => {
    const f = await fixture(original);
    try {
      const session = await f.start();
      const second = await f.attach("different-runtime");
      const sent: unknown[] = [];
      second.addEventListener("message", event => { sent.push(JSON.parse(String(event.data))); });
      for (const chars of ["", "must not reach process 1\n"]) {
        await expect(f.router.execute("write_stdin", { session_id: session, chars }, f.context(`poll-${chars.length}`)))
          .resolves.toMatchObject({ success: false, structuredResult: { status: "unavailable", reason: "process_runtime_replaced" },
            output: expect.stringContaining("cannot be routed to the replacement runtime") });
      }
      expect(sent).toEqual([]);
    } finally { await f.close(); }
  });

  it("does not bind a completed exec receipt to a replacement runtime", async () => {
    const f = await fixture("runtime-one");
    try {
      await f.start();
      const second = await f.attach("runtime-two");
      const sent: unknown[] = [];
      second.addEventListener("message", event => { sent.push(JSON.parse(String(event.data))); });
      // Reuse the original effect identity after its completed receipt survived
      // the socket. A refreshed exec route must not relabel old process 1.
      await expect(f.router.execute("exec_command", { cmd: "fixture-command", workdir: "/session-hand" }, f.context("start")))
        .resolves.toMatchObject({ success: false, structuredResult: { status: "ambiguous" } });
      expect(sent).toEqual([]);
    } finally { await f.close(); }
  });

  it("binds an exec refreshed before admission to the runtime that actually started it", async () => {
    const f = await fixture("runtime-one");
    try {
      const second = await f.attach("runtime-two");
      // The namespace captures the old snapshot; account routing refreshes exec.
      const session = await f.start(second);
      const frame = nextFrame(second);
      const poll = f.router.execute("write_stdin", { session_id: session }, f.context("poll"));
      await f.finish(second, await frame, { exit_code: 0 });
      await expect(poll).resolves.toMatchObject({ success: true });
    } finally { await f.close(); }
  });
});

describe("Hand receipt reconciliation after managed->account response loss", () => {
  const LOST = "Network connection lost to https://account-tools.internal/invoke token=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  type Drop = "after_delivery" | "before_delivery" | "truncated" | undefined;

  async function journey(drop: Drop, loseReceipts = false) {
    const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
    const owner = crypto.randomUUID();
    const stub = namespace.getByName(owner);
    const response = await stub.fetch("https://account-tools.internal/tool-host", {
      headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
    });
    const host = response.webSocket!;
    acceptHostSocket(host);
    // Every broker frame the synthetic Hand receives, in order.
    const frames: Record<string, unknown>[] = [];
    const waiters: Array<() => void> = [];
    host.addEventListener("message", event => { frames.push(JSON.parse(String(event.data))); waiters.splice(0).forEach(wake => wake()); });
    const frame = async (type: string, index = 0) => {
      while (frames.filter(entry => entry.type === type).length <= index) {
        await new Promise<void>((resolve, reject) => {
          const timer = setTimeout(() => reject(new Error(`no ${type} frame: ${JSON.stringify(frames)}`)), 5_000);
          waiters.push(() => { clearTimeout(timer); resolve(); });
        });
      }
      return frames.filter(entry => entry.type === type)[index]!;
    };
    host.send(JSON.stringify({
      type: "catalog", capabilities: ["turn_metadata"], attachment_id: "laptop", tools: [machineEntry()],
      machines: [{ id: "laptop", name: "Laptop", workspace: "/work", capabilities: ["shell"] }],
    }));
    await frame("ready");
    // Account RPC paths in order, and the transport that loses the first /invoke response.
    const paths: string[] = [];
    let pendingDrop = drop;
    const lossy = { getByName: (name: string) => {
      const target = namespace.getByName(name);
      return { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = new URL(input instanceof Request ? input.url : String(input)).pathname;
        paths.push(path);
        if (path === "/invoke-receipt" && loseReceipts) throw new TypeError(LOST);
        if (path !== "/invoke" || pendingDrop === undefined) return target.fetch(input, init);
        const mode = pendingDrop; pendingDrop = undefined;
        if (mode === "before_delivery") throw new TypeError(LOST);
        if (mode === "truncated") {
          // The account answered, but its body was cut off in transit.
          const answered = await target.fetch(input, init);
          await answered.body?.cancel();
          return new Response("{", { headers: { "content-type": "application/json" } });
        }
        // Deliver, wait until the Hand holds the call, then lose the connection.
        const connection = new AbortController();
        const forwarded = target.fetch(input, { ...init, signal: connection.signal });
        forwarded.catch(() => {});
        await frame("call");
        connection.abort();
        throw new TypeError(LOST);
      } };
    } } as unknown as DurableObjectNamespace<AccountHostedTools>;
    const provider = new AccountHostedToolsProvider(lossy, owner, () => true);
    await provider.refresh();
    const complete = (callId: unknown, output: string) => host.send(JSON.stringify({
      type: "result", call_id: callId, outcome: { status: "completed", output: {
        output, success: true, structured_result: { output, exit_code: 0, wall_time_seconds: 0 }, metadata: null, process_trace: null,
      } },
    }));
    return { stub, owner, host, frames, frame, paths, provider, complete };
  }

  it("recovers a completed result through a receipt-only read without cancelling or resending", async () => {
    const { frame, frames, paths, provider, complete } = await journey("after_delivery");
    const tool = provider.machineTool("laptop", "exec_command")!;
    const result = tool.handler({ cmd: "echo once", workdir: "/laptop" }, { sessionId: "agent", callId: "completed-loss" });
    const call = await frame("call");
    complete(call.call_id, "ran once");
    await expect(result).resolves.toMatchObject({ success: true, output: "ran once" });
    expect(paths.filter(path => path === "/invoke")).toHaveLength(1);
    expect(paths).toContain("/invoke-receipt");
    expect(frames.filter(entry => entry.type === "call")).toHaveLength(1);
    expect(frames.filter(entry => entry.type === "cancel")).toEqual([]);
  });

  it("waits on a still-running call's original runtime and returns its later result", async () => {
    const { frame, frames, paths, provider, complete } = await journey("after_delivery");
    const result = provider.machineTool("laptop", "exec_command")!.handler(
      { cmd: "sleep 1; echo done", workdir: "/laptop" }, { sessionId: "agent", callId: "running-loss" });
    const call = await frame("call");
    await vi.waitFor(() => expect(paths).toContain("/invoke-receipt"), { timeout: 5_000 });
    // The connection loss did not cancel the running command on the Hand.
    expect(frames.filter(entry => entry.type === "cancel")).toEqual([]);
    complete(call.call_id, "finished later");
    await expect(result).resolves.toMatchObject({ success: true, output: "finished later" });
    expect(paths.filter(path => path === "/invoke")).toHaveLength(1);
    expect(frames.filter(entry => entry.type === "call")).toHaveLength(1);
  });

  it("delivers explicit cancellation to the Hand and never polls a receipt", async () => {
    const { frame, frames, paths, provider } = await journey(undefined);
    const turn = new AbortController();
    const result = provider.machineTool("laptop", "exec_command")!.handler(
      { cmd: "sleep 60", workdir: "/laptop" }, { sessionId: "agent", callId: "explicit-cancel", signal: turn.signal });
    const call = await frame("call");
    turn.abort();
    await expect(result).resolves.toMatchObject({ success: false,
      structuredResult: { status: "ambiguous", reason: "cancelled_after_dispatch", cancel: "requested", resent: false } });
    await expect(frame("cancel")).resolves.toMatchObject({ type: "cancel", call_id: call.call_id });
    expect(paths).toContain("/cancel-invocation");
    expect(paths).not.toContain("/invoke-receipt");
    expect(frames.filter(entry => entry.type === "call")).toHaveLength(1);
  });

  it("fences a never-delivered call so a late duplicate can never run, and explains a safe retry", async () => {
    const { stub, owner, frames, paths, provider } = await journey("before_delivery");
    const input = { cmd: "touch once", workdir: "/laptop" };
    const result = await provider.machineTool("laptop", "exec_command")!.handler(input, { sessionId: "agent", callId: "lost-before-delivery" });
    expect(result).toMatchObject({ success: false,
      structuredResult: { status: "unavailable", admitted: false, resent: false, reason: "receipt_missing" } });
    const message = (result as { output: string }).output;
    expect(message).toContain("environment");
    expect(message).toContain("TypeError/network_lost: Network connection lost to [url]");
    expect(message).not.toContain("account-tools.internal");
    expect(message).not.toContain("AAAAAAAAAAAAAAAAAAAAAAAA");
    expect(paths.filter(path => path === "/invoke")).toHaveLength(1);
    // A delayed delivery of the original request is refused without dispatch.
    const late = await stub.fetch("https://account-tools.internal/invoke", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ owner_id: owner, name: "exec_command", input, session_id: "agent", call_id: "lost-before-delivery",
        machine_id: "laptop", route_token: provider.machineTool("laptop", "exec_command")!.routeToken }) });
    expect(late.status).toBe(200);
    expect(await late.json()).toMatchObject({ success: false, structured_result: { status: "cancelled" } });
    expect(frames.filter(entry => entry.type === "call")).toEqual([]);
  });

  it("recovers a result whose response body was truncated in transit", async () => {
    const { frame, frames, paths, provider, complete } = await journey("truncated");
    const result = provider.machineTool("laptop", "exec_command")!.handler(
      { cmd: "echo decoded", workdir: "/laptop" }, { sessionId: "agent", callId: "decode-loss" });
    complete((await frame("call")).call_id, "decoded once");
    await expect(result).resolves.toMatchObject({ success: true, output: "decoded once" });
    expect(paths.filter(path => path === "/invoke")).toHaveLength(1);
    expect(paths).toContain("/invoke-receipt");
    expect(frames.filter(entry => entry.type === "call")).toHaveLength(1);
  });

  it("gives up with outcome unknown, the sanitized cause and safe retry guidance when receipts stay unreachable", async () => {
    const { frame, frames, paths, provider, complete } = await journey("after_delivery", true);
    const tool = provider.machineTool("laptop", "exec_command")!;
    const context = { sessionId: "agent", callId: "unreachable-receipt" };
    const input = { cmd: "echo maybe", workdir: "/laptop" };
    const result = tool.handler(input, context);
    const call = await frame("call");
    const settled = await result as { success: boolean; output: string; structuredResult: Record<string, unknown> };
    expect(settled).toMatchObject({ success: false, structuredResult: { status: "ambiguous", admitted: "unknown", resent: false,
      reason: "receipt_unrecoverable", recovery: "receipt_unreachable", error: "TypeError/network_lost: Network connection lost to [url] [redacted]" } });
    expect(settled.output).toContain("Call environment to refresh Hand status");
    expect(settled.output).toContain("before deciding whether to retry it on the same Hand");
    expect(settled.output).toContain("Do not switch to SSH");
    expect(settled.output).not.toContain("account-tools.internal");
    expect(paths.filter(path => path === "/invoke-receipt")).toHaveLength(3);
    expect(paths.filter(path => path === "/invoke")).toHaveLength(1);
    // Transport loss never cancelled the command; it still finishes once.
    expect(frames.filter(entry => entry.type === "cancel")).toEqual([]);
    complete(call.call_id, "finished once");
    // A later same-identity replay reads the retained receipt instead of running again.
    await vi.waitFor(async () => {
      await expect(provider.machineTool("laptop", "exec_command")!.handler(input, context))
        .resolves.toMatchObject({ success: true, output: "finished once" });
    }, { timeout: 5_000 });
    expect(frames.filter(entry => entry.type === "call")).toHaveLength(1);
  });
});
