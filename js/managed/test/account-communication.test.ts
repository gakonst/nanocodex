import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import worker from "../src/index";
import { accountCommunication } from "../src/account-communication";
import type { Principal } from "../src/account-auth";

const owner = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const other = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const principal: Principal = {
  kind: "api_key", userId: owner, organizationId: owner, teamId: other,
  role: "owner", subjectId: `user:${owner}`, credentialId: "test", authorizationEpoch: 1,
  capabilities: ["agents:read", "tools:use"],
};
function fixture() {
  const execute = vi.fn(async (): Promise<unknown> => ({ configured: true, address: "agent@example.com", secret: "private" }));
  const config = { NANOCODEX_EMAIL: { execute }, NANOCODEX_EMAIL_OWNER_ID: owner, NANOCODEX_EMAIL_ADMIN_ID: owner,
    NANOCODEX_PHONE_OWNER_ID: owner, NANOCODEX_PHONE_ADMIN_ID: owner, TWILIO_VOICE_FROM_NUMBER: "+15551234567" };
  const call = (actor = principal, method = "GET", suffix = "") => worker.fetch(
    new Request("https://nanocodex.example/v1/account/communication" + suffix, { method }),
    { ...env, ...config } as unknown as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  return { execute, config, call };
}

describe("account communication", () => {
  it("returns only assigned addresses through an owner-scoped status RPC", async () => {
    const { call, execute } = fixture();
    const response = await call();
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ email: "agent@example.com", phone: "+15551234567" });
    expect(execute).toHaveBeenCalledExactlyOnceWith({ operation: "status", owner_id: owner, agent_id: "account-communication" });
  });
  it("isolates owners independently for each service and returns null for unassigned accounts", async () => {
    const { call, execute, config } = fixture();
    expect(await (await call({ ...principal, userId: other })).json()).toEqual({ email: null, phone: null });
    expect(execute).not.toHaveBeenCalled();
    expect(await accountCommunication({ ...config, NANOCODEX_PHONE_OWNER_ID: other }, owner))
      .toEqual({ email: "agent@example.com", phone: null });
    execute.mockClear();
    expect(await accountCommunication({ ...config, NANOCODEX_EMAIL_OWNER_ID: other }, owner))
      .toEqual({ email: null, phone: "+15551234567" });
    expect(execute).not.toHaveBeenCalled();
    expect(await accountCommunication({}, owner)).toEqual({ email: null, phone: null });
    expect(await accountCommunication(config, "")).toEqual({ email: null, phone: null });
  });
  it("requires the same service admin assignments as the email and phone tools", async () => {
    const { config, execute } = fixture();
    for (const admin of [undefined, other]) {
      expect(await accountCommunication({ ...config, NANOCODEX_EMAIL_ADMIN_ID: admin,
        NANOCODEX_PHONE_ADMIN_ID: admin }, owner)).toEqual({ email: null, phone: null });
    }
    expect(execute).not.toHaveBeenCalled();
  });
  it("rejects unauthenticated access, grants, missing capabilities, mutations, and owner selectors", async () => {
    const { call, execute } = fixture();
    expect((await call({ ...principal, connectGrant: { grantId: "test" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
    expect((await call({ ...principal, kind: "connect_grant" })).status).toBe(403);
    for (const capabilities of [[], ["agents:read"], ["tools:use"]] as Principal["capabilities"][]) {
      expect((await call({ ...principal, capabilities })).status).toBe(403);
    }
    expect((await call(principal, "POST")).status).toBe(405);
    expect((await call(principal, "GET", "?owner=" + other)).status).toBe(400);
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/account/communication"),
      env as Parameters<typeof worker.fetch>[1], createExecutionContext());
    expect(response.status).toBe(401);
    expect(execute).not.toHaveBeenCalled();
  });
  it("reports mailbox failure separately from unassigned resources without exposing errors", async () => {
    const { call, execute } = fixture();
    execute.mockRejectedValueOnce(new Error("provider credential private"));
    const response = await call();
    expect(response.status).toBe(503);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: "communication_unavailable" });
    for (const status of [null, {}, { configured: false, address: "agent@example.com" }, { status: "error" }, { configured: true, address: "bad\naddress" }]) {
      execute.mockResolvedValueOnce(status);
      expect((await call()).status).toBe(503);
    }
  });
  it("bounds mailbox latency and validates phone configuration", async () => {
    const { config, execute } = fixture();
    vi.useFakeTimers();
    try {
      execute.mockImplementationOnce(() => new Promise(() => {}));
      const pending = expect(accountCommunication(config, owner)).rejects.toThrow("unavailable");
      await vi.advanceTimersByTimeAsync(5_000);
      await pending;
    } finally { vi.useRealTimers(); }
    expect(await accountCommunication({ ...config, TWILIO_VOICE_FROM_NUMBER: "login-number" }, owner))
      .toEqual({ email: "agent@example.com", phone: null });
  });
});
