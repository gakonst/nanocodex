import { createExecutionContext, env } from "cloudflare:test";
import worker from "../src/index";
import { describe, expect, it, vi } from "vitest";
import { accountAdmin } from "../src/account-admin";
import type { Principal } from "../src/account-auth";

const owner = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const principal = { kind: "account_session", userId: owner, role: "owner" } as Principal;
const request = (method = "GET", suffix = "") => new Request("https://example.com/v1/account/admin" + suffix, { method });
function fixture() {
  const execute = vi.fn(async () => ({ configured: true, address: "agent@example.com", secret: "never-return" }));
  return { NANOCODEX_ADMIN_USER_ID: owner, NANOCODEX_EMAIL_OWNER_ID: owner, NANOCODEX_EMAIL_ADMIN_ID: owner,
    NANOCODEX_EMAIL: { execute }, NANOCODEX_PHONE_OWNER_ID: owner, NANOCODEX_PHONE_ADMIN_ID: owner, TWILIO_VOICE_FROM_NUMBER: "+15551234567" };
}
describe("account admin authorization", () => {
  it("enforces the gate through the managed Worker route", async () => {
    const call = (actor?: Principal) => worker.fetch(request(), { ...env, ...fixture() } as unknown as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor);
    expect((await call()).status).toBe(401);
    expect((await call({ ...principal, userId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" })).status).toBe(403);
    expect((await call(principal)).status).toBe(200);
  });
  it("fails closed for missing configuration, unauthenticated users, other owners, API keys and Connect grants", async () => {
    const config = fixture();
    expect((await accountAdmin(request(), config, undefined)).status).toBe(401);
    expect((await accountAdmin(request(), { ...config, NANOCODEX_ADMIN_USER_ID: undefined }, principal)).status).toBe(403);
    for (const actor of [
      { ...principal, userId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" },
      { ...principal, kind: "api_key" as const },
      { ...principal, connectGrant: { grantId: "grant" } as NonNullable<Principal["connectGrant"]> },
    ]) expect((await accountAdmin(request(), config, actor)).status).toBe(403);
    expect(config.NANOCODEX_EMAIL.execute).not.toHaveBeenCalled();
  });
  it("allows only read-only requests without selectors", async () => {
    const config = fixture();
    expect((await accountAdmin(request("POST"), config, principal)).status).toBe(405);
    expect((await accountAdmin(request("GET", "?owner=other"), config, principal)).status).toBe(400);
    expect(config.NANOCODEX_EMAIL.execute).not.toHaveBeenCalled();
  });
  it("returns only service configuration and assigned addresses, without caching or provider secrets", async () => {
    const response = await accountAdmin(request(), fixture(), principal);
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({
      email: { assigned: true, configured: true, available: true, address: "agent@example.com" },
      phone: { assigned: true, configured: true, address: "+15551234567" },
    });
  });
  it("does not expose contacts when service admin assignments are missing or differ", async () => {
    const config = fixture();
    for (const admin of [undefined, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"]) {
      const response = await accountAdmin(request(), { ...config, NANOCODEX_EMAIL_ADMIN_ID: admin,
        NANOCODEX_PHONE_ADMIN_ID: admin }, principal);
      expect(await response.json()).toEqual({
        email: { assigned: true, configured: true, available: false, address: null },
        phone: { assigned: true, configured: true, address: null },
      });
    }
    expect(config.NANOCODEX_EMAIL.execute).not.toHaveBeenCalled();
  });
  it("keeps service assignments independent from admin access and preserves phone on mailbox failure", async () => {
    const config = fixture();
    const other = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const unassigned = await accountAdmin(request(), { ...config, NANOCODEX_EMAIL_OWNER_ID: other, NANOCODEX_PHONE_OWNER_ID: other }, principal);
    expect(await unassigned.json()).toEqual({ email: { assigned: false, configured: true, available: false, address: null }, phone: { assigned: false, configured: true, address: null } });
    expect(config.NANOCODEX_EMAIL.execute).not.toHaveBeenCalled();
    config.NANOCODEX_EMAIL.execute.mockRejectedValueOnce(new Error("provider secret"));
    const failed = await accountAdmin(request(), config, principal);
    expect(await failed.json()).toEqual({ email: { assigned: true, configured: true, available: false, address: null }, phone: { assigned: true, configured: true, address: "+15551234567" } });
  });
});
