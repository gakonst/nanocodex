import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type AccountHostedTools, type DurableAgentSession } from "../src/index";
import type { Principal } from "../src/account-auth";
import { HAND_INVENTORY_DEADLINE_MS, WORKSPACE_INVENTORY_SESSION_LIMIT, WorkspaceHandRegistry, inventoryEntry } from "../src/hand-inventory";

const bindings = env as unknown as {
  NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools>;
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
};
function fixture() {
  const owner = crypto.randomUUID();
  const principal: Principal = { kind: "api_key", userId: owner, organizationId: crypto.randomUUID(),
    teamId: crypto.randomUUID(), role: "owner", subjectId: `user:${owner}`, credentialId: "fixture",
    authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
  const call = (actor: Principal | undefined = principal, method = "GET", suffix = "") => worker.fetch(
    new Request("https://nanocodex.example/v1/account/hands/inventory" + suffix, { method }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor);
  return { owner, principal, call, account: bindings.NANOCODEX_ACCOUNT_TOOLS.getByName(owner) };
}
function next(socket: WebSocket): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("catalog acknowledgement timed out")), 2000);
    socket.addEventListener("message", event => { clearTimeout(timer); resolve(JSON.parse(String(event.data))); }, { once: true });
  });
}
const machine = (id: string) => ({ id, name: id, workspace: "/private/fixture", capabilities: ["native"] });
async function publish(stub: DurableObjectStub, owner: string, id: string, principal?: Principal) {
  const response = await stub.fetch("https://fixture.internal/tool-host", {
    headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner, ...(principal ? {
      "x-nanocodex-session-organization-id": principal.organizationId,
      "x-nanocodex-session-team-id": principal.teamId, "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": JSON.stringify(principal.capabilities),
    } : {}) },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket!; socket.accept();
  const ready = next(socket);
  socket.send(JSON.stringify({ type: "catalog", capabilities: ["turn_metadata"], attachment_id: id, machines: [machine(id)], tools: [{
    provider: "native", remote_name: "device_info", parallel_safe: true, timeout_ms: 15000,
    definition: { type: "function", name: "device_info", description: "Fixture", strict: false,
      parameters: { type: "object", properties: {}, required: [], additionalProperties: false } },
  }] }));
  expect(await ready).toEqual({ type: "ready" });
  return socket;
}

it("public inventory retains offline account Hands and only returns safe owner fields", async () => {
  const f = fixture();
  const socket = await publish(f.account, f.owner, "account-device");
  try {
    const response = await f.call();
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ data: [{ id: "account-device", name: "account-device",
      kind: "hand", online: true, health: "connected" }], coverage: "known_account_and_workspace", complete: true });
    expect(await (await f.call({ ...f.principal, userId: crypto.randomUUID() })).json()).toEqual({
      data: [], coverage: "known_account_and_workspace", complete: true });
  } finally { socket.close(1000); }
  await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual([
    { id: "account-device", name: "account-device", kind: "hand", online: false, health: "offline" },
  ]);
});

it("public inventory enforces authentication, capabilities, Connect denial and request shape", async () => {
  const f = fixture();
  const response = await worker.fetch(new Request("https://nanocodex.example/v1/account/hands/inventory"),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext());
  expect(response.status).toBe(401);
  for (const capabilities of [[], ["agents:read"], ["tools:use"]] as const) {
    expect((await f.call({ ...f.principal, capabilities })).status).toBe(403);
  }
  expect((await f.call({ ...f.principal, connectGrant: { grantId: "fixture" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
  expect((await f.call(f.principal, "POST")).status).toBe(405);
  expect((await f.call(f.principal, "GET", "?owner=other")).status).toBe(400);
});

async function seedSession(f: ReturnType<typeof fixture>, id: string = crypto.randomUUID()) {
  const stub = bindings.NANOCODEX_SESSIONS.getByName(id);
  await runInDurableObject(stub, async (instance, state) => {
    const original = (instance as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(instance, "env", { configurable: true, value: { ...original,
      // Credential/catalog egress is external to this Hand inventory journey.
      // Keep the real Session router and broker; no model turn is submitted.
      NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
        const path = new URL(input instanceof Request ? input.url : String(input)).pathname;
        if (path.startsWith("/subjects/")) return new Response(null, { status: 204 });
        if (path.endsWith("/catalog")) return Response.json({ connectors: {}, mcp_connections: [] });
        if (path.endsWith("/credentials/vault")) return Response.json({ vault: [] });
        throw new Error("Unexpected inventory fixture egress: " + path);
      } },
    } });
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal','managed',?)`,
      id, f.owner, f.principal.organizationId, f.principal.teamId, Date.now());
  });
  return { id, stub };
}

it.each([
  ["legacy UUIDv4", "11111111-1111-4111-8111-111111111111"],
  ["current UUIDv7", "019b0000-0000-7000-8000-111111111111"],
  ["idempotent UUIDv8", "11111111-1111-8111-8111-111111111111"],
])("reads %s workspace publication through disconnect and reconnect at the public route", async (_, id) => {
  const f = fixture(), session = await seedSession(f, id);
  const socket = await publish(session.stub, f.owner, "workspace-device", f.principal);
  try {
    await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual([
      { id: "workspace-device", name: "workspace-device", kind: "workspace", online: true, health: "connected" },
    ]);
    expect(await session.stub.listWorkspaceHands(crypto.randomUUID())).toEqual({ data: [], complete: false });
  } finally { socket.close(1000); }
  await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual([]);
  await runInDurableObject(f.account, async (_, state) => {
    expect(new WorkspaceHandRegistry(state.storage).entries()).toHaveLength(0);
  });
  const reconnected = await publish(session.stub, f.owner, "workspace-device", f.principal);
  try {
    await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual([
      { id: "workspace-device", name: "workspace-device", kind: "workspace", online: true, health: "connected" },
    ]);
  } finally { reconnected.close(1000); }
});

it("recovers a workspace publication interrupted by an account broker reset without reconnecting", async () => {
  const f = fixture(), session = await seedSession(f);
  let original: unknown;
  let attempts = 0;
  await runInDurableObject(session.stub, async instance => {
    const internal = instance as unknown as { env: { NANOCODEX_ACCOUNT_TOOLS: typeof bindings.NANOCODEX_ACCOUNT_TOOLS } };
    original = internal.env.NANOCODEX_ACCOUNT_TOOLS;
    const namespace = internal.env.NANOCODEX_ACCOUNT_TOOLS;
    internal.env.NANOCODEX_ACCOUNT_TOOLS = { getByName: (name: string) => ({
      registerWorkspaceHands: (owner: string, id: string, entries: Parameters<AccountHostedTools["registerWorkspaceHands"]>[2]) => {
        if (++attempts === 1) throw new Error("Durable Object reset because its code was updated.");
        return namespace.getByName(name).registerWorkspaceHands(owner, id, entries);
      },
    }) } as typeof bindings.NANOCODEX_ACCOUNT_TOOLS;
  });
  const socket = await publish(session.stub, f.owner, "recovered-workspace", f.principal);
  try {
    await expect.poll(async () => (await (await f.call()).json() as any).data, { timeout: 5000 }).toEqual([
      { id: "recovered-workspace", name: "recovered-workspace", kind: "workspace", online: true, health: "connected" },
    ]);
    expect(attempts).toBeGreaterThan(1);
  } finally {
    await runInDurableObject(session.stub, async instance => {
      (instance as unknown as { env: { NANOCODEX_ACCOUNT_TOOLS: unknown } }).env.NANOCODEX_ACCOUNT_TOOLS = original;
    });
    socket.close(1000);
  }
}, 10_000);

it("filters current Connect rows and treats conflicting identity as unknown", async () => {
  const f = fixture(), session = await seedSession(f);
  await runInDurableObject(session.stub, async (_, state) => {
    for (const [route, id, grant] of [["account", "shared", null], ["connect", "shared", "grant"], ["private", "connect-only", "grant"]]) {
      state.storage.sql.exec(`INSERT INTO hosted_tool_routes(route_id,generation,host_id,catalog_json,machines_json,connect_grant_id)
        VALUES(?,1,'fixture','[]',?,?)`, route, JSON.stringify([machine(id!)]), grant);
    }
  });
  const entry = inventoryEntry(machine("shared"), false, true);
  await f.account.registerWorkspaceHands(f.owner, session.id, [entry]);
  const result = await (await f.call()).json();
  expect(result).toEqual({ data: [{ ...entry, online: null, health: "unknown" }], coverage: "known_account_and_workspace", complete: false });
  await runInDurableObject(session.stub, async (_, state) => {
    state.storage.sql.exec("UPDATE hosted_tool_routes SET connect_grant_id='grant' WHERE route_id='account'");
  });
  expect(await (await f.call()).json()).toEqual({ data: [], coverage: "known_account_and_workspace", complete: true });
});

it("retains unknown identity when workspace owner/discovery is unavailable", async () => {
  const f = fixture(), id = crypto.randomUUID();
  await f.account.registerWorkspaceHands(f.owner, id, [inventoryEntry(machine("retained"), true, true)]);
  expect(await (await f.call()).json()).toEqual({ data: [{ id: "retained", name: "retained", kind: "workspace",
    online: null, health: "unknown" }], coverage: "known_account_and_workspace", complete: false });
  expect(await f.account.registerWorkspaceHands(crypto.randomUUID(), id, [])).toBe(false);
});

it("durably fences completeness at the registry bound without evicting retained identities", async () => {
  const f = fixture();
  await runInDurableObject(f.account, async (_, state) => {
    const registry = new WorkspaceHandRegistry(state.storage);
    for (let i = 0; i < WORKSPACE_INVENTORY_SESSION_LIMIT; i++) {
      expect(registry.register(crypto.randomUUID(), [inventoryEntry(machine(`retained-${i}`), false, true)])).toBe(true);
    }
    expect(registry.register(crypto.randomUUID(), [inventoryEntry(machine("overflow"), true, true)])).toBe(false);
    expect(new WorkspaceHandRegistry(state.storage).complete).toBe(false);
  });
  const result = await (await f.call()).json() as any;
  expect(result.complete).toBe(false);
  expect(result.data).toHaveLength(WORKSPACE_INVENTORY_SESSION_LIMIT);
  expect(result.data.every((entry: any) => entry.online === null && entry.health === "unknown")).toBe(true);
  expect(HAND_INVENTORY_DEADLINE_MS).toBeLessThan(6000);
});

it("bounds hung discovery below the CLI timeout and limits concurrent session reads", async () => {
  const f = fixture();
  for (let i = 0; i < 12; i++) {
    await f.account.registerWorkspaceHands(f.owner, crypto.randomUUID(), [inventoryEntry(machine(`hung-${i}`), true, true)]);
  }
  let original: unknown;
  let active = 0, peak = 0;
  const finish: (() => void)[] = [];
  await runInDurableObject(f.account, async instance => {
    const internal = instance as unknown as { env: { NANOCODEX_SESSIONS: unknown } };
    original = internal.env.NANOCODEX_SESSIONS;
    internal.env.NANOCODEX_SESSIONS = { getByName: () => ({ listWorkspaceHands: () => {
      peak = Math.max(peak, ++active);
      return new Promise(resolve => finish.push(() => resolve({ data: [], complete: false })));
    } }) };
  });
  try {
    const started = Date.now();
    const result = await (await f.call()).json() as any;
    expect(Date.now() - started).toBeLessThan(5500);
    expect(peak).toBe(8);
    expect(result.complete).toBe(false);
    expect(result.data).toHaveLength(12);
    expect(result.data.every((entry: any) => entry.online === null && entry.health === "unknown")).toBe(true);
  } finally {
    for (const resolve of finish) resolve();
    await runInDurableObject(f.account, async instance => {
      (instance as unknown as { env: { NANOCODEX_SESSIONS: unknown } }).env.NANOCODEX_SESSIONS = original;
    });
  }
}, 15_000);

it("keeps a reconnect discoverable while public inventory polls overlap disconnection", async () => {
  const f = fixture(), session = await seedSession(f);
  let socket = await publish(session.stub, f.owner, "racing-device", f.principal);
  const expected = [{ id: "racing-device", name: "racing-device", kind: "workspace", online: true, health: "connected" }];
  try {
    await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual(expected);
    for (let attempt = 0; attempt < 5; attempt++) {
      const beforeClose = f.call();
      socket.close(1000);
      const duringClose = f.call();
      socket = await publish(session.stub, f.owner, "racing-device", f.principal);
      await Promise.all([beforeClose, duringClose]);
      await expect.poll(async () => (await (await f.call()).json() as any).data).toEqual(expected);
    }
  } finally { socket.close(1000); }
}, 10_000);

it("reclaims owner-verified deleted sessions without disclosing them to other owners", async () => {
  const f = fixture(), session = await seedSession(f);
  await f.account.registerWorkspaceHands(f.owner, session.id, [inventoryEntry(machine("ended"), true, true)]);
  await runInDurableObject(session.stub, async (_, state) => {
    state.storage.sql.exec(`INSERT INTO session_initialization_ownership(singleton,session_id,owner_id,runtime_profile,state)
      VALUES(1,?,?,'managed','deleted')`, session.id, f.owner);
  });
  expect(await session.stub.listWorkspaceHands(crypto.randomUUID())).toEqual({ data: [], complete: false });
  expect(await (await f.call()).json()).toEqual({ data: [], coverage: "known_account_and_workspace", complete: true });
  await runInDurableObject(f.account, async (_, state) => {
    expect(new WorkspaceHandRegistry(state.storage).entries()).toHaveLength(0);
  });
});

it("reuses a full registry slot after a definitively disconnected session is polled", async () => {
  const f = fixture(), ended = await seedSession(f);
  await runInDurableObject(f.account, async (_, state) => {
    const registry = new WorkspaceHandRegistry(state.storage);
    registry.register(ended.id, [inventoryEntry(machine("ended"), true, true)]);
    for (let i = 1; i < WORKSPACE_INVENTORY_SESSION_LIMIT; i++)
      registry.register(crypto.randomUUID(), [inventoryEntry(machine(`unknown-${i}`), true, true)]);
  });
  const result = await (await f.call()).json() as any;
  expect(result.data).toHaveLength(WORKSPACE_INVENTORY_SESSION_LIMIT - 1);
  expect(result.complete).toBe(false);
  const fresh = await seedSession(f);
  const socket = await publish(fresh.stub, f.owner, "new-publisher", f.principal);
  try {
    await expect.poll(async () => (await (await f.call()).json() as any).data
      .some((entry: any) => entry.id === "new-publisher" && entry.online === true)).toBe(true);
  } finally { socket.close(1000); }
});

it("retires only the observed disconnected pre-runtime generation through the public API", async () => {
  const f = fixture();
  const owner: Principal = { ...f.principal, capabilities: ["agents:read", "agents:write", "tools:use"] };
  const api = (body?: unknown, actor: Principal | undefined = owner) => worker.fetch(
    new Request("https://nanocodex.example/v1/account/hand-relays" + (body === undefined ? "" : "/retire"), {
      method: body === undefined ? "GET" : "POST",
      ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
    }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor);
  const connect = async () => {
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/account/tool-host", {
      headers: { upgrade: "websocket" },
    }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), owner);
    expect(response.status).toBe(101);
    const socket = response.webSocket!;
    socket.accept();
    const ready = next(socket);
    socket.send(JSON.stringify({ type: "catalog", capabilities: ["turn_metadata"], attachment_id: "pre-runtime", machines: [machine("pre-runtime")], tools: [{
      provider: "native", remote_name: "device_info", parallel_safe: true, timeout_ms: 15000,
      definition: { type: "function", name: "device_info", description: "Fixture", strict: false,
        parameters: { type: "object", properties: {}, required: [], additionalProperties: false } },
    }] }));
    expect(await ready).toEqual({ type: "ready" });
    return socket;
  };
  const status = async () => (await (await api()).json() as any).legacy;
  let socket = await connect();
  try {
    const [observed] = await status();
    expect(observed).toMatchObject({ machine_id: "pre-runtime", runtime_id: null, online: true, pending_calls: 0, retirable: false });
    expect(Number.isSafeInteger(observed.generation)).toBe(true);
    expect(observed.generation).toBeGreaterThan(0);
    const target = { machine_id: observed.machine_id, runtime_id: null, generation: observed.generation };
    expect(await (await api(target)).json()).toEqual({ error: "legacy_runtime_still_connected" });
    socket.close(1000);
    await expect.poll(async () => (await status())[0].retirable).toBe(true);
    for (const generation of [undefined, 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, "1"]) {
      expect((await api({ ...target, generation })).status).toBe(400);
    }
    expect((await api({ ...target, extra: true })).status).toBe(400);
    expect((await api(target, f.principal)).status).toBe(403);
    expect((await api(target, { ...owner, connectGrant: { grantId: "fixture" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
    expect((await api(target, { ...owner, userId: crypto.randomUUID() })).status).toBe(409);
    const unauthenticated = await worker.fetch(new Request("https://nanocodex.example/v1/account/hand-relays/retire", {
      method: "POST", body: JSON.stringify(target),
    }), env as Parameters<typeof worker.fetch>[1], createExecutionContext());
    expect(unauthenticated.status).toBe(401);
    socket = await connect();
    socket.close(1000);
    await expect.poll(async () => (await status())[0].retirable).toBe(true);
    const [current] = await status();
    expect(current.generation).toBeGreaterThan(target.generation);
    expect(await (await api(target)).json()).toEqual({ error: "legacy_generation_changed" });
    const exact = { ...target, generation: current.generation };
    // Seed an unresolved historical ledger entry, preserving the real Worker,
    // broker, socket and public retirement transport throughout the journey.
    await runInDurableObject(f.account, async (_, state) => {
      state.storage.sql.exec(`INSERT INTO hosted_tool_calls(call_id,session_id,source_call_id,hand_id,host_runtime_id,
        host_id,lease_id,generation,model,name,input_json,output_token_budget,output_byte_budget,deadline_at,
        cancel_requested,state,created_at,updated_at)
        VALUES('legacy-pending','fixture','fixture','pre-runtime',NULL,'fixture','fixture',?,'fixture','fixture','{}',1,1,?,0,'admitted',1,1)`,
        current.generation, Date.now() + 60_000);
    });
    expect((await status())[0]).toMatchObject({ pending_calls: 1, retirable: false });
    expect(await (await api(exact)).json()).toEqual({ error: "legacy_runtime_has_pending_calls" });
    await runInDurableObject(f.account, async (_, state) => {
      state.storage.sql.exec("UPDATE hosted_tool_calls SET state='completed',result_json='{}' WHERE call_id='legacy-pending'");
    });
    const receipt = { retired: true, ...exact };
    expect(await (await api(exact)).json()).toEqual(receipt);
    expect(await (await api(exact)).json()).toEqual(receipt);
    expect(await status()).toEqual([]);
    expect((await (await f.call()).json() as any).data).toEqual([]);
    await runInDurableObject(f.account, async (_, state) => {
      expect(state.storage.sql.exec("SELECT state,result_json FROM hosted_tool_calls WHERE call_id='legacy-pending'").toArray())
        .toEqual([{ state: "completed", result_json: "{}" }]);
      expect(state.storage.sql.exec("SELECT machines_json,catalog_json FROM hosted_tool_routes").toArray())
        .toEqual([{ machines_json: null, catalog_json: null }]);
    });
    socket = await connect();
    expect((await api(exact)).status).toBe(409);
    expect((await status())[0]).toMatchObject({ online: true, retirable: false });
    expect((await (await f.call()).json() as any).data).toEqual([
      { id: "pre-runtime", name: "pre-runtime", kind: "hand", online: true, health: "connected" },
    ]);
    socket.close(1000);
    await expect.poll(async () => (await status())[0].retirable).toBe(true);
    expect((await api(exact)).status).toBe(409);
  } finally {
    socket.close(1000);
    await expect.poll(async () => (await (await f.call()).json() as any).data.some((entry: any) => entry.online === true)).toBe(false);
  }
}, 15_000);

it("retires an exact offline regional publication without removing a concurrent successor", async () => {
  const f = fixture();
  const owner: Principal = { ...f.principal, capabilities: ["agents:read", "agents:write", "tools:use"] };
  const testEnv = { ...env, NANOCODEX_REGIONAL_HAND_RELAYS: "true" } as Parameters<typeof worker.fetch>[1];
  const api = (body?: unknown, actor = owner) => worker.fetch(new Request(
    "https://nanocodex.example/v1/account/hand-relays" + (body === undefined ? "" : "/retire"), {
      method: body === undefined ? "GET" : "POST",
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), testEnv, createExecutionContext(), actor);
  const status = async () => (await (await api()).json() as any).regional;
  const open = (runtime: string) => worker.fetch(new Request("https://nanocodex.example/v1/account/tool-host", {
    headers: { upgrade: "websocket", "x-nanocodex-hand-machine-id": "regional-device", "x-nanocodex-hand-runtime-id": runtime },
    cf: { continent: "EU", country: "DE", longitude: "8.68" },
  }), testEnv, createExecutionContext(), owner);
  const connect = async (runtime: string) => {
    const response = await open(runtime);
    expect(response.status).toBe(101);
    const socket = response.webSocket!; socket.accept();
    const ready = next(socket);
    socket.send(JSON.stringify({ type: "catalog", capabilities: ["turn_metadata"], attachment_id: "regional-device",
      runtime_id: runtime, machines: [machine("regional-device")], tools: [{
        provider: "native", remote_name: "device_info", parallel_safe: true, timeout_ms: 15000,
        definition: { type: "function", name: "regional_device_info", description: "Fixture", strict: false,
          parameters: { type: "object", properties: {}, required: [], additionalProperties: false } },
      }] }));
    expect(await ready).toEqual({ type: "ready" });
    return socket;
  };
  const identity = (row: any) => ({ machine_id: row.machine_id, runtime_id: row.runtime_id,
    publication_id: row.publication_id, region: row.region });
  let runtime = crypto.randomUUID(), socket = await connect(runtime);
  try {
    const [live] = await status();
    expect(live).toMatchObject({ machine_id: "regional-device", runtime_id: runtime, region: "weur", online: true, retirable: false });
    const target = identity(live);
    expect((await api(target)).status).toBe(409);
    expect((await api(target, f.principal)).status).toBe(403);
    expect((await api(target, { ...owner, connectGrant: { grantId: "fixture" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
    expect((await api(target, { ...owner, userId: crypto.randomUUID() })).status).toBe(409);
    socket.close(1000);
    await expect.poll(async () => (await status())[0].retirable).toBe(true);
    expect((await api({ ...target, publication_id: crypto.randomUUID() })).status).toBe(409);
    const nextRuntime = crypto.randomUUID();
    const [retirement, successor] = await Promise.all([api(target), connect(nextRuntime)]);
    expect([200, 409]).toContain(retirement.status);
    socket = successor; runtime = nextRuntime;
    expect((await status())[0]).toMatchObject({ runtime_id: runtime, online: true, retirable: false });
    await api(target); // Idempotent old receipt or conflict; neither changes the successor.
    expect((await (await f.call()).json() as any).data).toEqual([
      { id: "regional-device", name: "regional-device", kind: "hand", online: true, health: "connected" },
    ]);
    socket.close(1000);
    await expect.poll(async () => (await status())[0].retirable).toBe(true);
    const exact = identity((await status())[0]);
    expect(await (await api(exact)).json()).toEqual({ retired: true, ...exact });
    expect(await (await api(exact)).json()).toEqual({ retired: true, ...exact });
    expect(await status()).toEqual([]);
    expect((await (await f.call()).json() as any).data).toEqual([]);
    expect((await open(runtime)).status).toBe(409);
    socket = await connect(crypto.randomUUID());
    await api(exact);
    expect((await status())[0]).toMatchObject({ online: true, retirable: false });
  } finally {
    socket.close(1000);
    await expect.poll(async () => (await (await f.call()).json() as any).data.some((entry: any) => entry.online === true)).toBe(false);
  }
}, 15_000);
