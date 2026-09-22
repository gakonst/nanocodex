import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { afterEach, expect, it, vi } from "vitest";
import type { EgressEnv } from "../src/egress";
import { UserConnectorBroker } from "../src/connector-broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import { decodeLinkDevice, decodeLinkToken, linkRequestAllowed, LINK_SCOPES } from "../src/connectors/link";

const workerEnv = env as unknown as EgressEnv;
afterEach(() => vi.restoreAllMocks());

it("connects Link devices, preserves exact accounts, requests approval, refreshes and revokes without exposing credentials", async () => {
  let now = Date.now();
  vi.spyOn(Date, "now").mockImplementation(() => now);
  let account = 0;
  let polls = 0;
  const calls: Request[] = [];
  vi.spyOn(globalThis, "fetch").mockImplementation(async input => {
    const request = new Request(input);
    calls.push(request.clone());
    const url = new URL(request.url);
    if (url.hostname === "login.link.com") {
      const form = new URLSearchParams(await request.text());
      if (url.pathname === "/device/code") {
        expect(form.get("scope")).toBe(LINK_SCOPES.join(" "));
        expect(form.get("connection_label")).toBe("Nanocodex");
        account++;
        return Response.json({ device_code: `secret-device-${account}`, user_code: "test-code",
          verification_uri_complete: "https://app.link.com/verify?code=test-code", expires_in: 600, interval: 5 });
      }
      if (url.pathname === "/device/revoke") return Response.json({});
      expect(url.pathname).toBe("/device/token");
      polls++;
      const refresh = form.get("grant_type") === "refresh_token";
      const id = (refresh ? form.get("refresh_token") : form.get("device_code"))!.split("-").at(-1);
      return Response.json({ access_token: `secret-access-${id}`, refresh_token: `secret-refresh-${id}`,
        token_type: "Bearer", expires_in: refresh ? 3600 : 1 });
    }
    expect(url.origin).toBe("https://api.link.com");
    const id = request.headers.get("authorization")!.split("-").at(-1);
    if (url.pathname === "/userinfo") return Response.json({ email: `buyer${id}@example.com` });
    if (url.pathname.endsWith("/request_approval")) {
      expect(request.method).toBe("POST");
      return Response.json({ id: "lsrq_123", approval_link: "https://app.link.com/approve/lsrq_123" });
    }
    return Response.json({ id: "lsrq_123", status: "created", account: id,
      card: { number: "sensitive-card" }, shared_payment_token: "sensitive-spt", nested: { link_pay_token: "sensitive-lpt" } });
  });
  const user = "link-owner";
  const status = async () => {
    const response = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
    expect(response.status).toBe(200);
    const text = await response.text();
    expect(text).not.toContain("secret-");
    return JSON.parse(text).connectors.link;
  };
  let lastAttempt = "";
  for (let i = 0; i < 2; i++) {
    const start = await SELF.fetch(`https://broker.test/users/${user}/connectors/link`, { method: "POST" });
    expect(start.status).toBe(200);
    const text = await start.text();
    expect(text).not.toContain("secret-device");
    expect(JSON.parse(text).authorization_url).toContain("https://app.link.com/");
    const attempt = JSON.parse(text).attempt;
    expect(attempt).toMatch(/^[A-Za-z0-9_-]{43}$/);
    if (lastAttempt) expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/link?attempt=${lastAttempt}`)).status).toBe(409);
    lastAttempt = attempt;
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors/link?attempt=${attempt}`)).json()).toEqual({ state: "pending", connected: false });
    const before = polls;
    await status();
    expect(polls).toBe(before); // Persisted provider polling interval.
    now += 5001;
    expect((await status()).connections).toHaveLength(i + 1);
    expect(await (await SELF.fetch(`https://broker.test/users/${user}/connectors/link?attempt=${attempt}`)).json()).toMatchObject({ state: "connected", connected: true });
  }
  const [alpha, beta] = (await status()).connections;
  const subject = "L".repeat(43);
  expect((await SELF.fetch(`https://broker.test/subjects/${subject}`, { method: "PUT",
    headers: { "content-type": "application/json" }, body: JSON.stringify({ user_id: user }) })).status).toBe(200);
  const base = { authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "x-nanocodex-subject": subject };
  expect((await SELF.fetch("https://api.link.com/spend_requests", { headers: base })).status).toBe(409);
  const headers = { ...base, "x-nanocodex-connector-connection": alpha.id, "content-type": "application/json" };
  const created = await SELF.fetch("https://api.link.com/spend_requests", { method: "POST", headers,
    body: JSON.stringify({ merchant_name: "Store", merchant_url: "https://store.example", amount: 100, currency: "usd",
      context: "The user asked Nanocodex to buy this item. This request asks the user to approve the exact purchase in their Link wallet.", test: true }) });
  expect(created.status).toBe(200);
  expect(await created.json()).toEqual({ id: "lsrq_123", status: "created", account: "1", nested: {} });
  const approval = await SELF.fetch("https://api.link.com/spend_requests/lsrq_123/request_approval", { method: "POST", headers });
  expect(await approval.json()).toMatchObject({ approval_link: "https://app.link.com/approve/lsrq_123" });
  const before = calls.length;
  for (const path of ["/spend_requests/create_delegated", "/spend_requests/lsrq_123/update_delegated", "/spend_requests/lsrq_123?include=card", "/spend_requests/lsrq_123/approve", "/device/token", "/spend_requests/%252e%252e/userinfo"]) {
    expect((await workerEnv.USER_CONNECTORS.getByName(user).fetch(`https://api.link.com${path}`, { method: "POST", headers })).status).toBe(403);
  }
  expect((await SELF.fetch("https://api.link.com/spend_requests", { method: "POST", headers, body: '{"approve":true}' })).status).toBe(400);
  expect(calls).toHaveLength(before);
  expect((await workerEnv.USER_CONNECTORS.getByName("other-link-owner").fetch("https://api.link.com/spend_requests", { headers })).status).toBe(404);
  await runInDurableObject(workerEnv.USER_CONNECTORS.getByName(user), async (_instance: UserConnectorBroker, state) => {
    const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("connector-state");
    expect(JSON.stringify(row)).not.toContain("secret-");
    const opened = await new CredentialVault(workerEnv, `connectors/${state.id}`).open<any>(row!.envelope);
    expect(opened.value.connections.link[alpha.id].refreshToken).toBe("secret-refresh-1");
    expect(opened.value.linkDevice).toBeUndefined();
  });
  expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/link/connections/${alpha.id}`, { method: "DELETE" })).status).toBe(204);
  expect((await status()).connections.map((value: { id: string }) => value.id)).toEqual([beta.id]);
  expect((await SELF.fetch("https://api.link.com/spend_requests", { headers })).status).toBe(404);
  expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/link/connections/${beta.id}`, { method: "DELETE" })).status).toBe(204);
  expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/link?attempt=${lastAttempt}`)).status).toBe(409);
});

it("persists Link backoff, recovers identity without reusing a device code and reports denied or expired attempts", async () => {
  let now = Date.now(), polls = 0, identities = 0;
  let tokenError: string | undefined = "slow_down";
  vi.spyOn(Date, "now").mockImplementation(() => now);
  vi.spyOn(globalThis, "fetch").mockImplementation(async input => {
    const url = new URL(new Request(input).url);
    if (url.pathname === "/device/code") return Response.json({ device_code: "private-code", user_code: "code",
      verification_uri_complete: "https://login.link.com/verify", expires_in: 60, interval: 5 });
    if (url.pathname === "/device/token") {
      polls++;
      return tokenError ? Response.json({ error: tokenError }, { status: 400 })
        : Response.json({ access_token: "access", refresh_token: "refresh", token_type: "Bearer", expires_in: 3600 });
    }
    expect(url.pathname).toBe("/userinfo");
    return ++identities === 1 ? new Response(null, { status: 503 }) : Response.json({ email: "recover@example.com" });
  });
  const base = "https://broker.test/users/link-backoff/connectors/link";
  const start = async () => (await (await SELF.fetch(base, { method: "POST" })).json() as { attempt: string }).attempt;
  const poll = async (attempt: string) => (await SELF.fetch(`${base}?attempt=${attempt}`)).json();
  const attempt = await start();
  expect(await start()).toBe(attempt);
  now += 5001;
  expect(await poll(attempt)).toMatchObject({ state: "pending" });
  now += 5001;
  await poll(attempt);
  expect(polls).toBe(1);
  tokenError = undefined;
  now += 5001;
  await poll(attempt);
  expect(polls).toBe(2);
  expect(identities).toBe(1);
  now += 10001;
  expect(await poll(attempt)).toMatchObject({ state: "connected" });
  expect(polls).toBe(2);
  tokenError = "access_denied";
  const denied = await start();
  now += 5001;
  expect(await poll(denied)).toEqual({ state: "denied", connected: false });
  const expired = await start();
  now += 60001;
  expect(await poll(expired)).toEqual({ state: "expired", connected: false });
});

it("validates Link protocol values and denies delegated spend authority", () => {
  expect(() => decodeLinkDevice({ device_code: "secret", user_code: "code", verification_uri_complete: "https://app.link.com.evil.test/verify", expires_in: 600 })).toThrow();
  expect(() => decodeLinkToken({ access_token: "secret", refresh_token: "secret", token_type: "Bearer", expires_in: 3600,
    scope: `${LINK_SCOPES.join(" ")} spend_requests:approve` })).toThrow();
  for (const method of ["DELETE", "PUT", "PATCH"]) expect(linkRequestAllowed(method, new URL("https://api.link.com/spend_requests/lsrq_123"))).toBe(false);
  expect(linkRequestAllowed("GET", new URL("https://api.link.com/spend_requests?include_history=true"))).toBe(true);
});
