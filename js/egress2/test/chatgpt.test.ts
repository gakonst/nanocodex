import { describe, expect, it, vi } from "vitest";
import { createEgressHandler, CREDENTIAL_PLACEHOLDER } from "../src/handler";
import { validChatGptImport } from "../src/chatgpt";
import { OwnerSubscription } from "../src/subscription";
import { CredentialCipher } from "../src/encryption";
import type { ChatGptSubscriptionHandle } from "nanocodex";
import { ChatGptSubscription } from "nanocodex/worker";
import { readFile } from "node:fs/promises";

const testKey = btoa("0123456789abcdef0123456789abcdef");
const cipher = (owner: string) => new CredentialCipher(testKey, owner);
const api = "https://api.openai.com/v1/responses";
const codex = "https://chatgpt.com/backend-api/codex/responses";
const request = (url = api, method = "POST", owner = "owner-a") => new Request(url, {
  method,
  headers: { "x-managed2-owner": owner, authorization: `Bearer ${CREDENTIAL_PLACEHOLDER}`,
    "chatgpt-account-id": "attacker", "x-openai-fedramp": "true",
    ...(method === "GET" ? { upgrade: "websocket" } : {}) },
  ...(method === "POST" ? { body: "{}" } : {}),
});
const jwt = (account: string, exp: number, fedramp = false) => {
  const data = JSON.stringify({ exp, "https://api.openai.com/auth": {
    chatgpt_account_id: account, chatgpt_account_is_fedramp: fedramp,
  } });
  return `eyJhbGciOiJub25lIn0.${btoa(data).replaceAll("=", "").replaceAll("+", "-").replaceAll("/", "_")}.signature`;
};

class FakeStorage {
  private readonly rows = new Map<string, unknown>();
  async get<T>(key: string): Promise<T | undefined> { return this.rows.get(key) as T | undefined; }
  async put(key: string, value: unknown) { this.rows.set(key, value); }
  async delete(key: string) { this.rows.delete(key); }
  async transaction<T>(fn: (tx: FakeStorage) => Promise<T>): Promise<T> { return fn(this); }
}

describe("ChatGPT subscription egress", () => {
  it("encrypts distinct randomized owner-bound envelopes; rejects plaintext, tampering and wrong keys", async () => {
    expect(() => new CredentialCipher("missing", "a")).toThrow(/CREDENTIAL_ENCRYPTION_KEY/);
    const vault = cipher("a");
    const one = await vault.seal("sk-synthetic", "openai");
    const two = await vault.seal("sk-synthetic", "openai");
    expect(one).toMatch(/^v1:/);
    expect(one).not.toContain("sk-synthetic");
    expect(one).not.toBe(two);
    expect(await vault.open(one, "openai")).toBe("sk-synthetic");
    await expect(vault.open("sk-synthetic", "openai")).rejects.toThrow();
    await expect(vault.open(one, "chatgpt")).rejects.toThrow();
    await expect(cipher("b").open(one, "openai")).rejects.toThrow();
    await expect(new CredentialCipher(btoa("z".repeat(32)), "a").open(one, "openai")).rejects.toThrow();
    await expect(vault.open(one.slice(0, -1) + (one.endsWith("A") ? "B" : "A"), "openai")).rejects.toThrow();
  });

  it("accepts exact matching import contract and rejects mismatched claims or expiration", () => {
    const expiry = 4_070_908_800_000; // 2099, synthetic only
    const imported = { access_token: jwt("account-a", expiry / 1000), refresh_token: "refresh-synthetic",
      account_id: "account-a", expires_at: expiry, fedramp: false };
    expect(validChatGptImport(imported)).toBe(true);
    expect(validChatGptImport({ ...imported, account_id: "account-b" })).toBe(false);
    // Codex auth.json can carry account_id outside the access token (e.g. id_token).
    const withoutAccountClaim = `e30.${btoa(JSON.stringify({ exp: expiry / 1000 }))
      .replaceAll("=", "").replaceAll("+", "-").replaceAll("/", "_")}.signature`;
    expect(validChatGptImport({ ...imported, access_token: withoutAccountClaim })).toBe(true);
    expect(validChatGptImport({ ...imported, extra: "field" })).toBe(false);
    expect(validChatGptImport({ ...imported, expires_at: 1 })).toBe(false);
  });

  it("routes the active subscription to exact Codex endpoint and replaces untrusted account headers", async () => {
    const upstreamFetch = vi.fn(async (out: Request) => {
      expect(out.url).toBe(codex);
      expect(out.headers.get("authorization")).toBe("Bearer synthetic-access");
      expect(out.headers.get("chatgpt-account-id")).toBe("real-account");
      expect(out.headers.has("x-openai-fedramp")).toBe(false);
      expect(out.headers.has("x-managed2-owner")).toBe(false);
      expect(out.headers.get("originator")).toBe("codex_cli_rs");
      expect(out.redirect).toBe("manual");
      return new Response("event: hello\\n\\n", { headers: { "content-type": "text/event-stream" } });
    });
    const proxy = createEgressHandler({
      readCredential: async () => ({ kind: "chatgpt" as const, secret: "synthetic-access", accountId: "real-account",
        fedramp: false, expiresAt: Date.now() + 3_600_000 }), upstreamFetch,
    });
    expect((await proxy.fetch(request(), {})).status).toBe(200);
    expect((await proxy.fetch(request(codex), {})).status).toBe(200);
    expect(upstreamFetch).toHaveBeenCalledTimes(2);
    expect((await proxy.fetch(request("https://chatgpt.com/backend-api/codex/other"), {})).status).toBe(403);
  });

  it("passes subscription GET Upgrade/101 and rejects API-key use on subscription endpoint", async () => {
    const switched = { status: 101, webSocket: { accept() {} } } as unknown as Response;
    const proxy = createEgressHandler({
      readCredential: async () => ({ kind: "chatgpt" as const, secret: "synthetic-access", accountId: "acct",
        fedramp: true, expiresAt: Date.now() + 3_600_000 }),
      upstreamFetch: async out => {
        expect(out.method).toBe("GET");
        expect(out.url).toBe(codex);
        expect(out.headers.get("upgrade")).toBe("websocket");
        expect(out.headers.get("x-openai-fedramp")).toBe("true");
        return switched;
      },
    });
    expect(await proxy.fetch(request(api, "GET"), {})).toBe(switched);
    const apiOnly = createEgressHandler({ readCredential: async () => "sk-synthetic" });
    expect((await apiOnly.fetch(request(codex), {})).status).toBe(403);
  });

  it("recovers rejected revision once and replays POST with rotated bearer, never on a second 401", async () => {
    const upstreamFetch = vi.fn(async (out: Request) => {
      const body = await out.text();
      expect(body).toBe("{}");
      expect(out.url).toBe(codex);
      expect(out.headers.get("chatgpt-account-id")).toBe("acct");
      return new Response(null, { status: 401 });
    });
    const recoverCredential = vi.fn(async (_owner: string, revision: string) => {
      expect(revision).toBe("1");
      return { kind: "chatgpt" as const, secret: "fresh-synthetic", accountId: "acct", fedramp: false,
        expiresAt: Date.now() + 3_600_000, revision: "2" };
    });
    const proxy = createEgressHandler({
      readCredential: async () => ({ kind: "chatgpt" as const, secret: "old-synthetic", accountId: "acct",
        fedramp: false, expiresAt: Date.now() + 3_600_000, revision: "1" }),
      recoverCredential, upstreamFetch,
    });
    expect((await proxy.fetch(request(), {})).status).toBe(401);
    expect(upstreamFetch).toHaveBeenCalledTimes(2);
    expect((upstreamFetch.mock.calls[0] as [Request])[0].headers.get("authorization")).toBe("Bearer old-synthetic");
    expect((upstreamFetch.mock.calls[1] as [Request])[0].headers.get("authorization")).toBe("Bearer fresh-synthetic");
    expect(recoverCredential).toHaveBeenCalledOnce();
  });

  it("refreshes a rejected subscription WebSocket handshake and returns its 101 upgrade", async () => {
    const switched = { status: 101, webSocket: { accept() {} } } as unknown as Response;
    const auths: string[] = [];
    const recoverCredential = vi.fn(async () => ({ kind: "chatgpt" as const,
      secret: "fresh-synthetic", accountId: "acct", fedramp: false,
      expiresAt: Date.now() + 3_600_000, revision: "2" }));
    const proxy = createEgressHandler({
      readCredential: async () => ({ kind: "chatgpt" as const, secret: "old-synthetic",
        accountId: "acct", fedramp: false, expiresAt: Date.now() + 3_600_000, revision: "1" }),
      recoverCredential,
      upstreamFetch: async (out: Request) => {
        expect(out.method).toBe("GET");
        expect(out.headers.get("upgrade")).toBe("websocket");
        auths.push(out.headers.get("authorization")!);
        return auths.length === 1 ? new Response(null, { status: 401 }) : switched;
      },
    });
    expect(await proxy.fetch(request(api, "GET"), {})).toBe(switched);
    expect(auths).toEqual(["Bearer old-synthetic", "Bearer fresh-synthetic"]);
    expect(recoverCredential).toHaveBeenCalledOnce();
  });

  it("rejects unencrypted Rust store payloads without migration", async () => {
    const storage = new FakeStorage();
    await storage.put("subscription", { revision: "1", payload: "synthetic-plaintext" });
    const store = new (await import("../src/subscription")).DurableSubscriptionStore(
      storage as unknown as DurableObjectStorage, cipher("owner"));
    await expect(store.load("owner")).rejects.toThrow(/Invalid encrypted credential/);
  });

  it("standard JS/WASM subscription refreshes via auth.openai.com and CAS-persists rotated tokens", async () => {
    const wasm = await (WebAssembly as unknown as { compile(bytes: Uint8Array): Promise<WebAssembly.Module> }).compile(
      await readFile(new URL("../../nanocodex/pkg-web/nanocodex_bg.wasm", import.meta.url).pathname));
    const storage = new FakeStorage();
    const store = new (await import("../src/subscription")).DurableSubscriptionStore(storage as unknown as DurableObjectStorage, cipher("synthetic-rotation"));
    const old = jwt("acct", Math.floor(Date.now() / 1000) + 10);
    const fresh = jwt("acct", Math.floor(Date.now() / 1000) + 3600);
    const upstream = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      expect(String(url)).toBe("https://auth.openai.com/oauth/token");
      expect(init?.redirect).toBe("manual");
      expect(JSON.parse(String(init?.body))).toEqual({
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann", grant_type: "refresh_token", refresh_token: "synthetic-refresh",
      });
      return Response.json({ access_token: fresh, refresh_token: "rotated-synthetic-refresh" });
    });
    const handle = await ChatGptSubscription.open({ module: wasm, id: "synthetic-rotation", store,
      seed: { accessToken: old, refreshToken: "synthetic-refresh", accountId: "acct" }, fetch: upstream });
    try {
      expect((await handle.credential()).accessToken).toBe(fresh);
      expect(upstream).toHaveBeenCalledOnce();
      const state = await store.load("synthetic-rotation");
      expect(state.revision).toBe("2");
      expect(state.payload).toContain("rotated-synthetic-refresh");
      const raw = await storage.get<{ revision: string; payload: string }>("subscription");
      expect(raw?.payload).toMatch(/^v1:/);
      expect(raw?.payload).not.toContain("synthetic-refresh");
      expect(raw?.payload).not.toContain(fresh);
    } finally { handle.dispose(); }
  });

  it("keeps the active encrypted credential when a replacement fails after staging", async () => {
    const storage = new FakeStorage();
    let rejectReplacement = false;
    const open = async ({ store, seed }: { store: import("nanocodex").ChatGptSubscriptionStore;
      seed?: import("nanocodex").ChatGptCredentialSeed }) => {
      if (seed) {
        const before = await store.load("owner");
        await store.compareAndSwap("owner", { expectedRevision: before.revision, payload: seed.accessToken });
        if (rejectReplacement) throw new Error("synthetic staging failure");
      }
      const token = (await store.load("owner")).payload;
      return { credential: async () => ({ kind: "chatgpt", accessToken: token, accountId: "acct", fedramp: false,
        revision: "1" }), status: async () => ({ state: "authenticated", expiresAt: Date.now() + 3_600_000 }),
        dispose: () => {} } as unknown as ChatGptSubscriptionHandle;
    };
    const manager = new OwnerSubscription(storage as unknown as DurableObjectStorage, "owner", open, cipher("owner"));
    const imported = { access_token: jwt("acct", 4_070_908_800), refresh_token: "synthetic-refresh",
      account_id: "acct", expires_at: 4_070_908_800_000, fedramp: false };
    await manager.replace(imported);
    const first = await manager.credential();
    rejectReplacement = true;
    await expect(manager.replace({ ...imported, access_token: jwt("acct", 4_070_908_801) }))
      .rejects.toThrow("synthetic staging failure");
    expect((await manager.credential()).secret).toBe(first.secret);
    const stored = await storage.get<{ payload: string }>("subscription");
    expect(stored?.payload).toMatch(/^v1:/);
    expect(stored?.payload).not.toContain(first.secret);
  });

  it("explicit import replaces old subscription; store CAS revisions and no old seed survives", async () => {
    const storage = new FakeStorage();
    let current = "";
    const dispose = vi.fn();
    const open = vi.fn(async ({ store, seed }: { store: import("nanocodex").ChatGptSubscriptionStore;
      seed?: import("nanocodex").ChatGptCredentialSeed }) => {
      const before = await store.load("owner");
      if (seed) {
        expect(before.payload).toBeUndefined();
        current = seed.accessToken;
        expect((await store.compareAndSwap("owner", { expectedRevision: before.revision, payload: current })).status).toBe("committed");
      } else current = before.payload ?? "";
      return { credential: async () => ({ kind: "chatgpt", accessToken: current, accountId: "acct", fedramp: false,
        revision: "0" }), status: async () => ({ state: "authenticated", expiresAt: Date.now() + 3_600_000 }),
        dispose } as unknown as ChatGptSubscriptionHandle;
    });
    const manager = new OwnerSubscription(storage as unknown as DurableObjectStorage, "owner", open, cipher("owner"));
    const imported = { access_token: jwt("acct", 4_070_908_800), refresh_token: "synthetic-refresh",
      account_id: "acct", expires_at: 4_070_908_800_000, fedramp: false };
    await manager.replace(imported);
    expect((await manager.credential()).secret).toBe(imported.access_token);
    await manager.replace({ ...imported, access_token: jwt("acct", 4_070_908_801), expires_at: 4_070_908_801_000 });
    expect((await manager.credential()).secret).not.toBe(imported.access_token);
    const raw = await storage.get<{ payload: string }>("subscription");
    expect(raw?.payload).toMatch(/^v1:/);
    expect(raw?.payload).not.toContain(imported.access_token);
  });
});
