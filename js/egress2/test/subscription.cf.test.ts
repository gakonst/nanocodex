import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { UserCredentials } from "../src/index";

const credentialEnv = env as unknown as { USER_CREDENTIALS: DurableObjectNamespace<UserCredentials> };
const jwt = (exp: number) => `e30.${btoa(JSON.stringify({ exp, "https://api.openai.com/auth": {
  chatgpt_account_id: "synthetic-account", chatgpt_account_is_fedramp: false,
} })).replaceAll("=", "").replaceAll("+", "-").replaceAll("/", "_")}.signature`;

describe("actual workerd UserCredentials + compiled Rust subscription", () => {
  it("routes only the authenticated owner's credential and strips internal headers", async () => {
    const owner = `synthetic-api-${crypto.randomUUID()}`;
    const stub = credentialEnv.USER_CREDENTIALS.getByName(owner);
    await stub.putCredential("openai", "sk-synthetic-only");
    const request = (id: string, authorization = "Bearer NANOCODEX_PROVIDER_CREDENTIAL") => SELF.fetch(
      "https://api.openai.com/v1/responses", {
        method: "POST", headers: { "x-managed2-owner": id, "x-nanocodex-subject": "private-user",
          authorization, "content-type": "application/json" }, body: "{}",
      });
    const response = await request(owner);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ authorized: true, leakedOwner: false, leakedSubject: false });
    expect((await request(`unknown-${crypto.randomUUID()}`)).status).toBe(403);
    expect((await request(owner, "Bearer wrong")).status).toBe(400);
  });

  it("reports safe timing and validates correlation through the Worker boundary", async () => {
    const owner = `synthetic-timing-${crypto.randomUUID()}`;
    await credentialEnv.USER_CREDENTIALS.getByName(owner).putCredential("openai", "sk-synthetic-only");
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      const send = (trace: string) => SELF.fetch("https://api.openai.com/v1/responses", {
        method: "POST", headers: { "x-managed2-owner": owner, "x-managed2-trace-id": trace,
          "x-nanocodex-egress-request-id": "caller-must-not-pass-through",
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" },
        body: "{}",
      });
      const trace = "fc7b7fd4-48e8-4ccb-bfe2-058128da0131";
      const first = await send(trace);
      expect(first.status).toBe(200);
      expect(first.headers.get("server-timing")).toMatch(/egress_credential;dur=\d+\.\d, .*egress_upstream_headers;dur=\d+\.\d, .*egress_route;desc="openai_api", egress_cache;desc="miss"/);
      expect(await first.json()).toMatchObject({ authorized: true });
      const second = await send("invalid-trace");
      expect(second.status).toBe(200);
      expect(second.headers.get("server-timing")).toContain('egress_cache;desc="hit"');
      expect(await second.json()).toMatchObject({ authorized: true });
      const events = info.mock.calls.map(([event]) => event).filter((event) =>
        typeof event === "object" && event !== null && "event" in event && event.event === "responses_egress");
      expect(events).toHaveLength(2);
      expect(events[0]).toMatchObject({ trace_id: trace, egress_request_id: null,
        route_kind: "openai_api", response_status: 200, upstream_status: 200,
        credential_cache: "miss", recovery_outcome: "not_attempted", recovery_ms: null, retry_attempt: 0 });
      expect(events[1]).toMatchObject({ trace_id: null, egress_request_id: null,
        credential_cache: "hit", recovery_outcome: "not_attempted", retry_attempt: 0 });
      for (const event of events as Record<string, unknown>[]) {
        expect(event.credential_ms).toEqual(expect.any(Number));
        expect(event.upstream_headers_ms).toEqual(expect.any(Number));
        expect(JSON.stringify(event)).not.toContain(owner);
        expect(JSON.stringify(event)).not.toContain("caller-must-not-pass-through");
      }
    } finally { info.mockRestore(); }
  });

  it("keeps search parsing and credential failures sanitized through the Worker boundary", async () => {
    const owner = `synthetic-failure-${crypto.randomUUID()}`;
    const stub = credentialEnv.USER_CREDENTIALS.getByName(owner);
    await stub.putCredential("openai", "sk-synthetic-only");
    const headers = { "x-managed2-owner": owner,
      "x-managed2-trace-id": "fc7b7fd4-48e8-4ccb-bfe2-058128da0131",
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" };
    const malformed = await SELF.fetch("https://nanocodex.internal/v1/search", {
      method: "POST", headers, body: '{"private-synthetic-query":',
    });
    expect(malformed.status).toBe(400);
    expect(await malformed.json()).toEqual({ error: "invalid_search_request" });
    await runInDurableObject(stub, async (_instance, state) => {
      state.storage.sql.exec("UPDATE credentials SET value = ? WHERE provider = ?", "private-synthetic-corruption", "openai");
    });
    const unavailable = await SELF.fetch("https://api.openai.com/v1/responses", {
      method: "POST", headers, body: "{}",
    });
    expect(unavailable.status).toBe(502);
    expect(await unavailable.text()).toBe("Credential unavailable");
  });

  it("encrypts API keys in SQLite and rejects plaintext without fallback", async () => {
    const stub = credentialEnv.USER_CREDENTIALS.getByName(`synthetic-api-${crypto.randomUUID()}`);
    await stub.putCredential("openai", "sk-synthetic-only");
    const stored = await runInDurableObject(stub, async (_instance, state) => {
      const row = state.storage.sql.exec<{ value: string }>("SELECT value FROM credentials WHERE provider = ?", "openai").toArray()[0];
      return row?.value;
    });
    expect(stored).toMatch(/^v1:/);
    expect(stored).not.toContain("sk-synthetic-only");
    expect(await stub.getActiveCredential()).toEqual({ kind: "openai", secret: "sk-synthetic-only" });
    await runInDurableObject(stub, async (_instance, state) => {
      state.storage.sql.exec("UPDATE credentials SET value = ? WHERE provider = ?", "sk-synthetic-only", "openai");
    });
    // Catch within workerd; rejected RPC promises otherwise surface as pool unhandled rejections.
    const rejected = await runInDurableObject(stub, async instance => {
      try { await instance.getActiveCredential(); return false; }
      catch { return true; }
    });
    expect(rejected).toBe(true);
  });

  it("imports and rotates a rejected revision through the DO with no live network", async () => {
    const owner = `synthetic-owner-${crypto.randomUUID()}`;
    const stub = credentialEnv.USER_CREDENTIALS.getByName(owner);
    // Import a future token, then exercise the Rust lifecycle's
    // explicit rejected-revision recovery.
    const futureExpiry = 4_070_908_800_000;
    await stub.putChatGptCredential({ access_token: jwt(futureExpiry / 1000), refresh_token: "synthetic-refresh",
      account_id: "synthetic-account", expires_at: futureExpiry, fedramp: false });
    await runInDurableObject(stub, async (_instance, state) => {
      const row = await state.storage.get<{ revision: string; payload: string }>("subscription");
      expect(row?.payload).toMatch(/^v1:/);
      expect(row?.payload).not.toContain("synthetic-refresh");
      expect(row?.payload).not.toContain("synthetic-account");
      const sql = state.storage.sql.exec<{ value: string }>("SELECT value FROM credentials WHERE provider = ?", "active").toArray();
      expect(sql[0]?.value).toBe("chatgpt");
    });
    const initial = await stub.getActiveCredential();
    expect(initial?.kind).toBe("chatgpt");
    if (initial?.kind !== "chatgpt") throw Error("subscription unavailable");
    const recovered = await stub.recoverChatGptCredential(initial.revision!);
    expect(recovered?.kind).toBe("chatgpt");
    if (recovered?.kind !== "chatgpt") throw Error("recovery unavailable");
    expect(recovered.revision).not.toBe(initial.revision);
    expect(recovered.secret).not.toBe(initial.secret);
    await runInDurableObject(stub, async (_instance, state) => {
      const row = await state.storage.get<{ payload: string }>("subscription");
      expect(row?.payload).toMatch(/^v1:/);
      expect(row?.payload).not.toContain("synthetic-rotated");
    });
    const reread = await stub.getActiveCredential();
    expect(reread?.kind).toBe("chatgpt");
    if (reread?.kind === "chatgpt") expect(reread.secret).toBe(recovered.secret);
    const replacement = jwt(futureExpiry / 1000 - 60);
    await stub.putChatGptCredential({ access_token: replacement, refresh_token: "replacement-synthetic",
      account_id: "synthetic-account", expires_at: futureExpiry - 60_000, fedramp: false });
    const swapped = await stub.getActiveCredential();
    expect(swapped?.kind).toBe("chatgpt");
    if (swapped?.kind === "chatgpt") {
      expect(swapped.secret).toBe(replacement);
      expect(swapped.revision).not.toBe(recovered.revision);
    }
    const rejected = await runInDurableObject(stub, async instance => {
      try {
        await instance.putChatGptCredential({ access_token: replacement, refresh_token: "bad-replacement",
          account_id: "wrong-account", expires_at: futureExpiry - 60_000, fedramp: false });
        return false;
      } catch { return true; }
    });
    expect(rejected).toBe(true);
    const afterRejection = await stub.getActiveCredential();
    expect(afterRejection?.kind).toBe("chatgpt");
    if (afterRejection?.kind === "chatgpt") expect(afterRejection.secret).toBe(replacement);
  });
});

describe("subscription relay through real Worker and fixture Durable Objects", () => {
  it("routes model, search and upgrade traffic through the owner's single relay", async () => {
    const owner = `synthetic-relay-${crypto.randomUUID()}`;
    const future = 4_070_908_800_000;
    await credentialEnv.USER_CREDENTIALS.getByName(owner).putChatGptCredential({
      access_token: jwt(future / 1000), refresh_token: "synthetic-refresh",
      account_id: "synthetic-account", expires_at: future, fedramp: false,
    });
    const headers = { "x-managed2-owner": owner, authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL" };
    const model = await SELF.fetch("https://api.openai.com/v1/responses", {
      method: "POST", headers: { ...headers, "content-type": "application/json" }, body: "{}",
    });
    expect(model.status).toBe(200);
    expect(await model.json()).toEqual({ relay: "relay" });
    const search = await SELF.fetch("https://nanocodex.internal/v1/search", {
      method: "POST", headers: { ...headers, "content-type": "application/json" },
      body: JSON.stringify({ session_id: "synthetic", commands: { search_query: [{ q: "fixture" }] } }),
    });
    expect(search.status).toBe(200);
    expect(await search.json()).toEqual({ output: "relay" });
    for (const budget of [0, 250_000, -1]) {
      const budgeted = await SELF.fetch("https://nanocodex.internal/v1/search", {
        method: "POST", headers: { ...headers, "content-type": "application/json" },
        body: JSON.stringify({ session_id: "synthetic", commands: { expected_output_budget: budget }, max_output_tokens: budget }),
      });
      expect(budgeted.status).toBe(budget < 0 ? 400 : 200);
    }
    const upgrade = await SELF.fetch("https://api.openai.com/v1/responses", {
      method: "GET", headers: { ...headers, upgrade: "websocket" },
    });
    expect(upgrade.status).toBe(101);
    expect(upgrade.webSocket).toBeDefined();
    upgrade.webSocket?.accept();
    upgrade.webSocket?.close();
  });
});
