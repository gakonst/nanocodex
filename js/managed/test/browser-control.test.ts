import { env, createExecutionContext, runInDurableObject } from "cloudflare:test";
import worker, { type DurableAgentSession } from "../src/index";
import type { Principal } from "../src/account-auth";
import { describe, expect, it, vi } from "vitest";
import { BrowserControl } from "../src/browser-control";
import type { ToolContext } from "nanocodex";

function fixture() {
  const values = new Map<string, unknown>();
  const storage = { get: vi.fn(async (key: string) => values.get(key)), put: vi.fn(async (key: string, value: unknown) => { values.set(key, value); }) };
  const info = vi.fn(async () => undefined);
  const create = () => new BrowserControl(storage as unknown as DurableObjectStorage, { fetch: vi.fn() }, info);
  return { create, values };
}
const context = (signal = new AbortController().signal) => ({ callId: "handoff-1", signal }) as ToolContext;
const request = (operation: string, body: unknown = {}) => new Request(`https://session.internal/browser/${operation}`, { method: "POST", body: JSON.stringify(body) });
const state = async (control: BrowserControl) => (await control.request(new Request("https://session.internal/browser"))).json() as Promise<{ mode: string; generation: string }>;

describe("cloud browser exclusive control", () => {
  it("persists takeover across reconstruction and fences stale releases", async () => {
    const { create } = fixture();
    const first = create();
    const taken = await (await first.request(request("takeover"))).json() as { generation: string };
    const restored = create();
    expect((await state(restored)).mode).toBe("human");
    expect((await restored.request(request("release", { generation: "stale" }))).status).toBe(409);
    expect((await restored.request(request("release", taken))).status).toBe(200);
    expect((await state(restored)).mode).toBe("agent");
    expect((await restored.request(request("type", { ...taken, text: "private" }))).status).toBe(409);
  });
  it("refuses takeover until an in-flight model operation completes", async () => {
    const control = fixture().create();
    let finish!: () => void;
    let entered!: () => void;
    const started = new Promise<void>((resolve) => { entered = resolve; });
    const running = control.model(context(), async () => { entered(); await new Promise<void>((resolve) => { finish = resolve; }); });
    await started;
    expect((await control.request(request("takeover"))).status).toBe(409);
    finish(); await running;
    expect((await control.request(request("takeover"))).status).toBe(200);
  });
  it("blocks model observation during human control and supports cancellation", async () => {
    const control = fixture().create();
    await control.request(request("takeover"));
    const abort = new AbortController();
    const run = vi.fn(async () => "must not run");
    const waiting = control.model(context(abort.signal), run);
    await state(control);
    abort.abort(new Error("cancelled"));
    await expect(waiting).rejects.toThrow("cancelled");
    expect(run).not.toHaveBeenCalled();
  });
  it("resumes handoff and does not repeat a completed call after restart", async () => {
    const { create } = fixture();
    const control = create();
    const waiting = control.handoff("Sign in to continue", context());
    const taken = await state(control);
    expect(taken.mode).toBe("human");
    await control.request(request("release", { generation: taken.generation }));
    expect(await waiting).toMatchObject({ status: "returned_to_agent" });
    expect(await create().handoff("Sign in to continue", context())).toMatchObject({ status: "returned_to_agent" });
  });
});

// Exercise the public Worker boundary, including tenant ownership, before any CDP work.
it("rejects Connect grants, insufficient capabilities, CSRF, and another account", async () => {
  const id = crypto.randomUUID();
  const principal: Principal = {
    kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner", subjectId: "user:11111111-1111-4111-8111-111111111111", credentialId: "test",
    authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
  };
  const namespace = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(namespace.getByName(id), async (_session, state) => {
    state.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
      VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`, id, principal.userId, principal.organizationId, principal.teamId, Date.now());
  });
  const call = (actor: Principal, origin?: string) => worker.fetch(new Request(`https://nanocodex.example/v1/agents/${id}/browser/takeover`, {
    method: "POST", body: "{}", headers: origin ? { origin } : {},
  }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor);
  expect((await call({ ...principal, capabilities: ["agents:read"] })).status).toBe(403);
  expect((await call({ ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } })).status).toBe(403);
  expect((await call({ ...principal, kind: "account_session" }, "https://evil.example")).status).toBe(403);
  expect((await call({ ...principal, userId: "22222222-2222-4222-8222-222222222222" })).status).toBe(404);
});
