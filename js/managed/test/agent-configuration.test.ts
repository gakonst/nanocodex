import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { configurationCatalog, networkAllows, parseConfiguration } from "../src/agent-configuration";
import { SessionOperations } from "../src/session-operations";
import { DurableEventLog } from "../src/durable-events";
import { createBrainWorkspace } from "../src/brain-workspace";
import { createBrainBucket } from "../src/brain-bucket";
import { createHmac } from "node:crypto";
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const inside = (fn: (state: DurableObjectState) => Promise<void>) => runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => fn(state));
const req = (path: string, method = "GET", body?: unknown) => new Request(`https://session.internal${path}`, { method, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });

describe("agent configuration", () => {
  it("validates explicit delegation without accepting contradictory or unbounded numeric values", () => {
    expect(parseConfiguration({}).multi_agent).toBeUndefined();
    expect(parseConfiguration({ multi_agent: { enabled: false } }).multi_agent).toEqual({ enabled: false });
    expect(parseConfiguration({ multi_agent: { enabled: true, max_concurrent_subagents: 2 } }).multi_agent).toEqual({ enabled: true, max_concurrent_subagents: 2 });
    for (const multi_agent of [{ enabled: false, max_concurrent_subagents: 2 }, { enabled: "false" },
      ...[0, -1, 1.5, 0x1_0000_0000, Number.MAX_SAFE_INTEGER + 1].map(max_concurrent_subagents => ({ enabled: true, max_concurrent_subagents }))])
      expect(() => parseConfiguration({ multi_agent })).toThrow();
  });
  it("validates workspace and exact-host policies without widening them", () => {
    for (const path of ["/brain/../secret", "/brain//x", "/etc/passwd", "/brain/x/./y", "/brain/x\0y"])
      expect(() => parseConfiguration({ environment: { files: [{ path, content: "x" }] } })).toThrow();
    expect(parseConfiguration({ environment: { files: [{ path: "/brain/skills/demo/SKILL.md", content: "fixture" }] } }).environment?.files).toHaveLength(1);
    const policy = { access: "restricted", allowed_domains: ["example.com"] } as const;
    expect(networkAllows({ ...policy, allowed_domains: [...policy.allowed_domains] }, "https://example.com/path")).toBe(true);
    for (const url of ["https://evil.example.com", "https://example.com.evil.com", "https://example.com:444", "https://user@example.com", "file:///etc/passwd"])
      expect(networkAllows({ ...policy, allowed_domains: [...policy.allowed_domains] }, url)).toBe(false);
    expect(networkAllows({ access: "disabled" }, "https://example.com")).toBe(false);
    expect(() => parseConfiguration({ environment: { network: { access: "restricted", allowed_domains: ["*.example.com"] } } })).toThrow();
  });
  it("retains immutable definitions and allows exact PUT retries", () => inside(async state => {
    const config = { instructions: "Use fixtures", tools: [] };
    expect((await configurationCatalog(req("/agent-definitions/reviewer", "PUT", config), state.storage)).status).toBe(201);
    expect((await configurationCatalog(req("/agent-definitions/reviewer", "PUT", config), state.storage)).status).toBe(200);
    expect((await configurationCatalog(req("/agent-definitions/reviewer", "PUT", { instructions: "changed" }), state.storage)).status).toBe(409);
    expect(await (await configurationCatalog(req("/agent-definitions/reviewer"), state.storage)).json()).toMatchObject({ configuration: config });
    await configurationCatalog(req("/agent-definitions/reviewer", "DELETE"), state.storage);
    expect((await configurationCatalog(req("/agent-definitions/reviewer"), state.storage)).status).toBe(404);
  }));
  it("rolls back webhook and usage projections with event admission, signs retries, and redacts secrets", () => inside(async state => {
    const ops = new SessionOperations(state.storage);
    const created = await (await ops.webhook(req("/webhook", "PUT", { url: "https://hooks.example.com/receive" }))).json<{ secret: string }>();
    const log = new DurableEventLog(state.storage, e => ops.record(e, "agent"));
    expect(() => state.storage.transactionSync(() => { log.append({ type: "turn_completed" }, "rolled-back"); throw new Error("rollback"); })).toThrow();
    expect(ops.nextAlarm()).toBeUndefined();
    const event = log.record({ type: "turn_completed" }, "turn");
    const bodies: string[] = [];
    await ops.drain((async (url, init) => {
      expect(url).toBe("https://hooks.example.com/receive"); expect(init?.redirect).toBe("manual");
      const headers = new Headers(init?.headers); const body = String(init?.body); bodies.push(body);
      expect(headers.get("webhook-signature")).toBe(`v1,${createHmac("sha256", created.secret).update(`${headers.get("webhook-id")}.${headers.get("webhook-timestamp")}.${body}`).digest("hex")}`);
      return new Response(null, { status: 503 });
    }) as typeof fetch);
    expect(ops.nextAlarm()).toBeGreaterThan(Date.now());
    state.storage.sql.exec("UPDATE managed_webhook_deliveries SET retry_at=0");
    await new SessionOperations(state.storage).drain((async (_, init) => { bodies.push(String(init?.body)); return new Response(null, { status: 204 }); }) as typeof fetch);
    expect(bodies[0]).toBe(bodies[1]); expect(ops.nextAlarm()).toBeUndefined();
    expect(await (await ops.webhook(req("/webhook"))).text()).not.toContain(created.secret);
    expect(await ops.usage("0").json()).toMatchObject({ data: [{ cursor: event.cursor, turn_id: "turn", usage: null }] });
  }));
  it("publishes immutable per-turn bytes, preserves unknown usage and fails oversized publication atomically", () => inside(async state => {
    const backing = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
    const workspace = createBrainWorkspace(createBrainBucket(state.storage, backing, "fixture"), "fixture");
    const ops = new SessionOperations(state.storage);
    await workspace.writeFile("/brain/outputs/result.txt", "first"); await ops.publish("turn-1", workspace);
    await ops.publish("deleted-turn", workspace, () => false);
    expect(state.storage.sql.exec("SELECT turn_id FROM managed_artifact_publications WHERE turn_id='deleted-turn'").toArray()).toEqual([]);
    expect((await ops.artifacts(req("/artifacts?turn_id=deleted-turn")).json<{ data: unknown[] }>()).data).toEqual([]);
    await workspace.writeFile("/brain/outputs/result.txt", "second"); await ops.publish("turn-1", workspace); await ops.publish("turn-2", workspace);
    const page = await ops.artifacts(req("/artifacts?turn_id=turn-1")).json<{ data: { id: string }[] }>();
    expect(await ops.artifacts(req(`/artifacts/${page.data[0]!.id}/content`)).text()).toBe("first");
    await workspace.writeFile("/brain/outputs/large.txt", "x".repeat(1_000_001)); await ops.publish("turn-3", workspace);
    expect(await ops.artifacts(req("/artifacts?turn_id=turn-3")).json()).toMatchObject({ data: [] });
    expect(state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_artifact_publications WHERE turn_id='turn-3'").one().state).toBe("failed");
  }));
});

it("authorizes operational routes before exposing another session or allowing a mutation", async () => {
  const { default: worker } = await import("../src/index");
  const { createExecutionContext } = await import("cloudflare:test");
  const principal: import("../src/account-auth").Principal = {
    kind: "api_key", userId: "11111111-1111-4111-8111-111111111111", organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", role: "owner", subjectId: "user:fixture", credentialId: "test",
    authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
  };
  const id = "0198d3f0-8844-7000-8000-000000000091";
  await runInDurableObject(sessions().getByName(id), async (_, state) => {
    state.storage.sql.exec(`INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,?,?,?,1,'https://nanocodex.example','managed',?)`, id, principal.userId, principal.organizationId, principal.teamId, Date.now());
  });
  const call = (resource: string, method = "GET", actor = principal, origin?: string) => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${id}/${resource}`, { method, headers: origin ? { origin } : {},
      ...(method === "PUT" ? { body: JSON.stringify({ url: "https://hooks.example.com" }) } : {}) }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  for (const resource of ["configuration", "environment", "usage", "usage/requests", "artifacts", "webhook", "required-actions"]) {
    expect((await call(resource)).status).toBe(200);
    expect((await call(resource, "GET", { ...principal, userId: "22222222-2222-4222-8222-222222222222" })).status).toBe(404);
    expect((await call(resource, "GET", { ...principal, capabilities: [] })).status).toBe(403);
    expect((await call(resource, "GET", { ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } })).status).toBe(403);
  }
  expect((await call("webhook", "PUT", { ...principal, kind: "account_session" }, "https://evil.example")).status).toBe(403);
  expect((await call("webhook", "PUT")).status).toBe(201);
  expect((await call("webhook", "DELETE")).status).toBe(204);
});

it("deduplicates model responses and filters child usage independently from root turn totals", () => inside(async state => {
  const ops = new SessionOperations(state.storage);
  const log = new DurableEventLog<{ type: string; event: { type: string; request_id: string; seq: number; payload: Record<string, unknown> }; agent_id: number }>(state.storage, e => ops.record(e, "agent"));
  const message = { type: "event", agent_id: 2, event: { type: "model.call.completed", request_id: "request", seq: 1,
    payload: { response_id: "response", duration_ns: 123_000_000, usage: { input_tokens: 100, output_tokens: 10 } } } };
  log.record(message, "turn"); log.record(message, "turn");
  expect(await ops.requests("0", "2").json()).toMatchObject({ data: [{ id: "response", agent_id: "2" }], has_more: false });
  expect((await ops.requests("0", "root").json<{ data: unknown[] }>()).data).toHaveLength(0);
  expect((await ops.requests("0", "2").json<{ data: unknown[] }>()).data).toHaveLength(1);
}));

it("marks interrupted setup as failed on reconstruction rather than replaying commands", () => inside(async state => {
  new SessionOperations(state.storage);
  state.storage.sql.exec("INSERT INTO managed_environment_setup VALUES (1, 'running', 3, NULL)");
  new SessionOperations(state.storage);
  expect(state.storage.sql.exec("SELECT state,step FROM managed_environment_setup").one()).toEqual({ state: "failed", step: 3 });
}));

it("prepares files, skills and actual embedded-shell commands once, under the network policy", () => inside(async state => {
  const { prepareEnvironment } = await import("../src/environment-setup");
  const { createManagedComputerRuntime } = await import("../src/computer-runtime");
  new SessionOperations(state.storage);
  const config = parseConfiguration({ environment: {
    network: { access: "disabled" }, files: [{ path: "/brain/input.txt", content: "fixture" }],
    skills: [{ name: "review", instructions: "Inspect fixtures" }], setup_commands: ["mkdir -p /brain/outputs && cat /brain/input.txt > /brain/outputs/result.txt"],
  } }).environment!;
  const workspace = createBrainWorkspace(createBrainBucket(state.storage, (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES, "setup"), "setup");
  let egressCalls = 0;
  const runtime = await createManagedComputerRuntime({ computer: {
    get fs(): never { throw new Error("must not open a hand"); }, [Symbol.dispose]() {},
  }, filesystem: workspace, networkPolicy: config.network, egress: { fetch: async () => { egressCalls++; return new Response("unexpected"); } } as unknown as Fetcher });
  try {
    const execute = async (cmd: string) => await runtime.tool.handler({ cmd }, {
      sessionId: "setup", callId: "setup", parentCallId: "", model: "fixture", signal: AbortSignal.timeout(5000),
    }) as { exit_code?: number; output?: string };
    await prepareEnvironment(state.storage, config, workspace, execute);
    expect(new TextDecoder().decode(await workspace.readFile("/brain/outputs/result.txt"))).toBe("fixture");
    expect(new TextDecoder().decode(await workspace.readFile("/brain/skills/review/SKILL.md"))).toBe("Inspect fixtures");
    await workspace.writeFile("/brain/input.txt", "changed");
    await prepareEnvironment(state.storage, config, workspace, () => { throw new Error("must not run twice"); });
    expect(new TextDecoder().decode(await workspace.readFile("/brain/input.txt"))).toBe("changed");
    await expect(runtime.fetch("https://example.com/")).rejects.toThrow("network policy");
    expect(egressCalls).toBe(0);
    expect(state.storage.sql.exec("SELECT state,step FROM managed_environment_setup").one()).toEqual({ state: "ready", step: 3 });
  } finally { runtime.dispose(); }
}));
