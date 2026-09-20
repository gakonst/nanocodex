import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import type { Principal } from "../src/account-auth";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";

async function fixture(resource = "challenge") {
  const id = crypto.randomUUID();
  const principal: Principal = {
    kind: "api_key", userId: crypto.randomUUID(), organizationId: crypto.randomUUID(),
    teamId: crypto.randomUUID(), role: "owner", subjectId: "api_key:test", credentialId: "test",
    authorizationEpoch: 1, capabilities: ["agents:write", "tools:use"],
  };
  const stub = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> })
    .NANOCODEX_SESSIONS.getByName(id);
  await runInDurableObject(stub, async (agent, state) => {
    expect((await agent.fetch(new Request("https://session.internal/credential-binding", {
      method: "PUT", body: JSON.stringify({ owner_id: principal.userId, session_id: id,
        subject: state.id.toString(), durability_import: null }),
    }))).status).toBe(204);
    expect((await agent.fetch(new Request("https://session.internal/initialize", {
      method: "PUT", body: JSON.stringify({ session_id: id, owner_id: principal.userId,
        organization_id: principal.organizationId, team_id: principal.teamId, authorization_epoch: 1,
        public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS, configuration: { tools: [] } }),
    }))).status).toBe(204);
    await state.storage.deleteAlarm();
  });
  const call = (actor = principal, init: RequestInit = {}, query = "") => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${id}/browser-vault/${resource}${query}`, {
      method: "POST", body: JSON.stringify({ challenge_id: "opaque-fixture", code: "123456" }),
      ...init, headers: { "content-type": "application/json", ...init.headers },
    }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  return { principal, call, stub };
}

describe("private browser direct challenge endpoint", () => {
  it("requires owner account capabilities and web same-origin authority", async () => {
    const { principal, call } = await fixture();
    for (const capabilities of [[], ["agents:write"], ["tools:use"]]) {
      expect((await call({ ...principal, capabilities } as Principal)).status).toBe(403);
    }
    expect((await call({ ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } })).status).toBe(403);
    expect((await call({ ...principal, userId: crypto.randomUUID() })).status).toBe(404);
    expect((await call({ ...principal, authorizationEpoch: 2 })).status).toBe(404);
    expect((await call({ ...principal, kind: "account_session" })).status).toBe(403);
    expect((await call({ ...principal, kind: "account_session" }, { headers: { origin: "https://evil.example" } })).status).toBe(403);
    expect((await call({ ...principal, kind: "account_session" }, { headers: { origin: "https://nanocodex.example" } })).status).toBe(409);
  });
  it("bounds and validates input without reflecting it", async () => {
    const { principal, call } = await fixture();
    const cases: [RequestInit, number][] = [
      [{ method: "GET", body: null }, 405],
      [{ headers: { "content-type": "text/plain" } }, 400],
      [{ body: "{" }, 400],
      [{ body: JSON.stringify({ challenge_id: "opaque", code: "123456", extra: true }) }, 400],
      [{ body: JSON.stringify({ challenge_id: "opaque", code: "x".repeat(129) }) }, 400],
      [{ body: " ".repeat(2049) }, 413],
      [{ body: "é".repeat(1025) }, 413],
    ];
    for (const [init, status] of cases) {
      const response = await call(principal, init);
      expect(response.status).toBe(status);
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(await response.text()).not.toContain("123456");
    }
    expect((await call(principal, {}, "?code=123456")).status).toBe(400);
    const unavailable = await call();
    expect(unavailable.status).toBe(409);
    expect(await unavailable.json()).toEqual({ error: "challenge_unavailable" });
  });
  it("rejects internal submissions lacking owner assertions", async () => {
    const { stub } = await fixture();
    const response = await stub.fetch("https://session.internal/browser-vault/challenge", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ challenge_id: "opaque-fixture", code: "123456" }),
    });
    expect(response.status).toBe(403);
    expect(await response.text()).not.toContain("123456");
  });
});


describe("private browser direct takeover endpoint", () => {
  it("uses the same owner, capability, and CSRF gates", async () => {
    const { principal, call } = await fixture("takeover");
    const init = { body: JSON.stringify({ challenge_id: "opaque-fixture", action: "observe" }) };
    expect((await call({ ...principal, capabilities: [] }, init)).status).toBe(403);
    expect((await call({ ...principal, userId: crypto.randomUUID() }, init)).status).toBe(404);
    expect((await call({ ...principal, kind: "account_session" }, init)).status).toBe(403);
    expect((await call({ ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } }, init)).status).toBe(403);
    const response = await call(principal, init);
    expect(response.status).toBe(409);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: "challenge_unavailable" });
  });
  it("accepts the native mobile contract through both HTTP boundaries", async () => {
    const { principal, call } = await fixture("takeover");
    for (const action of [
      { action: "observe", viewport: { width: 390, height: 700, mobile: true } },
      { action: "touch", phase: "start", x: 0.5, y: 0.5 },
      { action: "touch", phase: "move", x: 0.5, y: 0.2 },
      { action: "touch", phase: "end" },
      { action: "touch", phase: "cancel" },
      { action: "edit", delete_backward: 1, text: "synthetic🙂" },
      { action: "finish" },
    ]) {
      const response = await call(principal, { body: JSON.stringify({ challenge_id: "opaque-fixture", ...action }) });
      // No lease exists in this fixture. 409 proves parsing reached the runtime;
      // the obsolete route contract rejected these requests with 400.
      expect(response.status, JSON.stringify(action)).toBe(409);
      expect(await response.json()).toEqual({ error: "challenge_unavailable" });
    }
  });
  it("rejects arbitrary or oversized action input before invoking runtime", async () => {
    const { principal, call } = await fixture("takeover");
    for (const action of [
      { action: "evaluate", code: "private-text" },
      { action: "observe", text: "private-text" },
      { action: "observe", viewport: { width: 390, height: 700, mobile: true, secret: "private-text" } },
      { action: "observe", viewport: { width: 239, height: 700, mobile: true } },
      { action: "touch", phase: "start", x: 0.5 },
      { action: "touch", phase: "other" },
      { action: "edit", delete_backward: 129, text: "private-text" },
      { action: "edit", delete_backward: 0, text: "x".repeat(513) },
      { action: "click", x: 2, y: 0.5 },
      { action: "click", x: 0.5 },
      { action: "type", text: "x".repeat(513) },
      { action: "key", key: "F12" },
      { action: "scroll", delta_y: 2001 },
      { action: "finish", image: "private-text" },
    ]) {
      const response = await call(principal, { body: JSON.stringify({ challenge_id: "opaque-fixture", ...action }) });
      expect(response.status).toBe(400);
      expect(await response.text()).not.toContain("private-text");
    }
    expect((await call(principal, { body: "x".repeat(2049) })).status).toBe(413);
  });
});
