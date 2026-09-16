import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

import { type ChatGptCredentialImport, UserCredentialBroker } from "../src/broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const REFRESH_EARLY_MS = 5 * 60_000;

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("Service-Binding-only ChatGPT credential import", () => {
  it("preserves an existing refresh alarm across activation without writing it again", async () => {
    const user = "restore-existing-refresh-alarm";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const imported = importedCredential("restored-account");
    expect((await importThroughControl(user, imported)).status).toBe(204);
    await runInDurableObject(stub, async (_instance, state) => {
      // A persisted alarm earlier than the normal refresh time must not be postponed.
      const existing = Date.now() + 60_000;
      await state.storage.setAlarm(existing);
      const set = vi.spyOn(state.storage, "setAlarm");
      const remove = vi.spyOn(state.storage, "deleteAlarm");
      const restored = new UserCredentialBroker(state, workerEnv);
      expect((await restored.resolveModelCredential(false)).status).toBe(200);
      expect(await state.storage.getAlarm()).toBe(existing);
      expect(set).not.toHaveBeenCalled();
      expect(remove).not.toHaveBeenCalled();
    });
  });

  it("repairs a missing refresh alarm when restoring credentials", async () => {
    const user = "restore-missing-refresh-alarm";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const imported = importedCredential("missing-alarm-account");
    expect((await importThroughControl(user, imported)).status).toBe(204);
    await runInDurableObject(stub, async (_instance, state) => {
      await state.storage.deleteAlarm();
      const restored = new UserCredentialBroker(state, workerEnv);
      expect((await restored.resolveModelCredential(false)).status).toBe(200);
      expect(await state.storage.getAlarm()).toBe(imported.expires_at - REFRESH_EARLY_MS);
    });
  });

  it("returns live subscription snapshots through RPC and observes revocation", async () => {
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const user = "rpc-credential-subscription";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    expect(await stub.resolveModelCredential(false)).toMatchObject({ status: 404, credential: null, resolve_ms: expect.any(Number) });
    expect((await importThroughControl(user, importedCredential("rpc-account"))).status).toBe(204);
    const first = await stub.resolveModelCredential(false);
    expect(first).toMatchObject({ status: 200, credential: { kind: "chatgpt", accountId: "rpc-account" } });
    expect(first.resolve_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(info).toHaveBeenCalledWith({
      type: "egress.credential.rpc", resolve_id: first.resolve_id, status: 200,
      queue_ms: expect.any(Number), operation_ms: expect.any(Number),
      activation_ms: expect.any(Number), activation_age_ms: expect.any(Number),
    });
    expect(first.credential).toEqual((await internalCredential(stub)).body);
    // A recovery for an old revision must use the newer credential, not refresh it.
    expect((await stub.resolveModelCredential(true, -1)).credential).toEqual(first.credential);
    expect((await stub.fetch("https://credentials.internal/v1/chatgpt", { method: "DELETE" })).status).toBe(204);
    expect(await stub.resolveModelCredential(false)).toMatchObject({ status: 404, credential: null });
  });

  it("accepts only the exact bounded five-field document", async () => {
    const valid = importedCredential("bounds-account");
    const invalid: unknown[] = [
      { ...valid, unknown: "field" },
      { ...valid, fedramp: undefined },
      { ...valid, access_token: ` ${valid.access_token}` },
      { ...valid, access_token: "not.a.jwt.with.too.many.parts" },
      { ...valid, refresh_token: ` ${valid.refresh_token}` },
      { ...valid, refresh_token: `${valid.refresh_token}\n` },
      { ...valid, refresh_token: "r".repeat(32 * 1024 + 1) },
      { ...valid, account_id: "a".repeat(257) },
      { ...valid, expires_at: valid.expires_at + 1_000 },
      importedCredential("bounds-account", { expiresInMs: REFRESH_EARLY_MS - 1_000 }),
      importedCredential("different-wire-account", {
        accessAccount: "access-account",
      }),
      importedCredential("bounds-account", { accessFedramp: true }),
    ];

    for (const [index, body] of invalid.entries()) {
      const response = await importThroughControl(`invalid-${index}`, body);
      expect(response.status, `invalid fixture ${index}`).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_chatgpt_credential" });
    }

    const wrongContentType = await SELF.fetch(
      "https://broker.internal/users/wrong-content-type/credentials/chatgpt",
      { method: "PUT", body: JSON.stringify(valid) },
    );
    expect(wrongContentType.status).toBe(400);
    expect(await wrongContentType.json()).toEqual({ error: "invalid_chatgpt_credential" });

    const oversized = await SELF.fetch(
      "https://broker.internal/users/oversized/credentials/chatgpt",
      {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ access_token: "x".repeat(65 * 1024) }),
      },
    );
    expect(oversized.status).toBe(413);
    expect(await oversized.json()).toEqual({ error: "body_too_large" });
  });

  it("restores committed credentials when an RPC refresh claim cannot be persisted", async () => {
    const user = "rpc-refresh-write-failure";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const imported = importedCredential("rpc-refresh-account");
    expect((await importThroughControl(user, imported)).status).toBe(204);
    await runInDurableObject(stub, async (instance: UserCredentialBroker, state) => {
      const write = vi.spyOn(state.storage, "put").mockRejectedValueOnce(new Error("injected write failure"));
      try {
        expect(await instance.resolveModelCredential(true, 0))
          .toMatchObject({ status: 503, credential: null });
      } finally { write.mockRestore(); }
      expect(await instance.resolveModelCredential(false))
        .toMatchObject({ status: 200, credential: { secret: imported.access_token, revision: 0 } });
    });
  });

  it("preserves an opaque refresh token in encrypted missing state", async () => {
    const user = "encrypted-import";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const login = await stub.fetch("https://credentials.internal/v1/chatgpt/login/start", {
      method: "POST",
    });
    expect(login.status).toBe(200);

    const imported = importedCredential("encrypted-account", {
      expiresInMs: 60 * 60_000,
      refreshToken: "opaque::refresh/token+bytes=kept.exactly~",
    });
    const response = await importThroughControl(user, imported);
    expect(response.status).toBe(204);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("pragma")).toBe("no-cache");
    expect(await response.text()).toBe("");

    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
      expect(row).toBeDefined();
      const raw = JSON.stringify(row);
      expect(raw).toContain("ciphertext");
      expect(raw).not.toContain(imported.access_token);
      expect(raw).not.toContain(imported.refresh_token);
      expect(raw).not.toContain(imported.account_id);

      const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
      const opened = await vault.open<{
        active: string;
        login?: unknown;
        chatgpt: {
          accessToken: string;
          refreshToken: string;
          accountId: string;
          expiresAt: number;
          revision: number;
        };
      }>(row!.envelope);
      expect(opened.value).toMatchObject({
        active: "chatgpt",
        chatgpt: {
          accessToken: imported.access_token,
          refreshToken: imported.refresh_token,
          accountId: imported.account_id,
          expiresAt: imported.expires_at,
          revision: 0,
        },
      });
      expect(opened.value.login).toBeUndefined();
      expect(await state.storage.getAlarm()).toBe(imported.expires_at - REFRESH_EARLY_MS);
    });
  });

  it("keeps a healthy same-account credential as an idempotent no-op", async () => {
    const user = "same-account-import";
    const retained = importedCredential("same-account", {
      expiresInMs: 2 * 60 * 60_000,
      marker: "retained-newer",
    });
    const staleReplay = importedCredential("same-account", {
      expiresInMs: 60 * 60_000,
      marker: "stale-replay",
    });

    expect((await importThroughControl(user, retained)).status).toBe(204);
    expect((await importThroughControl(user, staleReplay)).status).toBe(204);
    const snapshot = await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user));
    expect(snapshot).toMatchObject({
      status: 200,
      body: { secret: retained.access_token, revision: 0, accountId: "same-account" },
    });
  });

  it("retains different accounts and deduplicates concurrent imports", async () => {
    const user = "concurrent-import";
    const first = importedCredential("concurrent-a", { marker: "concurrent-secret-a" });
    const second = importedCredential("concurrent-b", { marker: "concurrent-secret-b" });
    const responses = await Promise.all([
      importThroughControl(user, first), importThroughControl(user, second), importThroughControl(user, first),
    ]);
    expect(responses.map((response) => response.status)).toEqual([204, 204, 204]);
    const status = await SELF.fetch(`https://broker.internal/users/${user}/credentials`);
    const body = await status.text();
    expect(body).not.toMatch(/concurrent-secret|refreshToken|accessToken/);
    expect(JSON.parse(body).chatgpt.accounts.map((account: { account_id: string }) => account.account_id).sort())
      .toEqual(["concurrent-a", "concurrent-b"]);
  });

  it("switches once for concurrent limit reports, persists cooldowns, and recovers after reset", async () => {
    const user = "cooldown-import";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const apiKey = await stub.fetch("https://credentials.internal/v1/openai-key", {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ api_key: "sk-do-not-use-paid-fallback" }),
    });
    expect(apiKey.status).toBe(204);
    const first = importedCredential("cooldown-a");
    const second = importedCredential("cooldown-b");
    await importThroughControl(user, first);
    await importThroughControl(user, second);
    const resetAt = Date.now() + 60_000;
    const report = (account: string, revision: number) => stub.fetch("https://credentials.internal/v1/chatgpt/limit", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ account_id: account, revision, reset_at: resetAt }),
    });
    for (const response of await Promise.all([report("cooldown-b", 1), report("cooldown-b", 1)])) {
      expect(await response.json()).toEqual({ available: true });
    }
    expect(await internalCredential(stub)).toMatchObject({ body: { accountId: "cooldown-a" } });
    // Reimporting a live auth file must not clear the account's cooldown.
    await importThroughControl(user, second);
    expect(await internalCredential(stub)).toMatchObject({ body: { accountId: "cooldown-a" } });
    expect(await (await report("cooldown-a", 0)).json()).toEqual({ available: false });
    expect(await internalCredential(stub)).toEqual({ status: 429, body: { error: "chatgpt_accounts_exhausted" } });
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
      const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
      const stored = (await vault.open<{ chatgpt: { limitedUntil: number }; chatgptBackups: { limitedUntil: number }[] }>(row!.envelope)).value;
      expect(stored.chatgpt.limitedUntil).toBe(resetAt);
      expect(stored.chatgptBackups[0].limitedUntil).toBe(resetAt);
    });
    vi.spyOn(Date, "now").mockReturnValue(resetAt + 1);
    expect(await internalCredential(stub)).toMatchObject({ status: 200, body: { accountId: "cooldown-a" } });
  });

  it("refreshes every account independently and accepts limits from an older live socket", async () => {
    const user = "refresh-pool";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const first = importedCredential("refresh-a", { expiresInMs: 10 * 60_000, refreshToken: "refresh-a-token" });
    const second = importedCredential("refresh-b", { expiresInMs: 10 * 60_000, refreshToken: "refresh-b-token" });
    await importThroughControl(user, first);
    await importThroughControl(user, second);
    vi.spyOn(Date, "now").mockReturnValue(first.expires_at - 4 * 60_000);
    const refreshed: string[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
      const body = JSON.parse(String(init?.body));
      refreshed.push(body.refresh_token);
      return Response.json({
        access_token: jwt({ exp: Math.ceil(Date.now() / 1_000) + 3600 }),
        refresh_token: `${body.refresh_token}-rotated`,
      });
    });
    await runInDurableObject(stub, async (instance: UserCredentialBroker) => instance.alarm());
    expect(refreshed.sort()).toEqual(["refresh-a-token", "refresh-b-token"]);
    expect(await internalCredential(stub)).toMatchObject({ body: { accountId: "refresh-b", revision: 2 } });
    const report = await stub.fetch("https://credentials.internal/v1/chatgpt/limit", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ account_id: "refresh-b", revision: 1, reset_at: Date.now() + 60_000 }),
    });
    expect(await report.json()).toEqual({ available: true });
    expect(await internalCredential(stub)).toMatchObject({ body: { accountId: "refresh-a", revision: 3 } });
  });

  it("ignores stale limit reports and disconnects the whole pool", async () => {
    const user = "stale-limit-import";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    await importThroughControl(user, importedCredential("stale-a"));
    await importThroughControl(user, importedCredential("stale-b"));
    const response = await stub.fetch("https://credentials.internal/v1/chatgpt/limit", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ account_id: "stale-b", revision: 0, reset_at: Date.now() + 60_000 }),
    });
    expect(await response.json()).toEqual({ available: false });
    expect(await internalCredential(stub)).toMatchObject({ body: { accountId: "stale-b" } });
    expect((await stub.fetch("https://credentials.internal/v1/chatgpt", { method: "DELETE" })).status).toBe(204);
    expect(await internalCredential(stub)).toMatchObject({ status: 404 });
    const status = await SELF.fetch(`https://broker.internal/users/${user}/credentials`);
    expect(await status.json()).toMatchObject({ chatgpt: { connected: false, accounts: [] } });
    await importThroughControl(user, importedCredential("stale-b"));
    const stale = await stub.fetch("https://credentials.internal/v1/chatgpt/limit", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ account_id: "stale-b", revision: 1, reset_at: Date.now() + 60_000 }),
    });
    expect(await stale.json()).toEqual({ available: false });
    expect(await internalCredential(stub)).toMatchObject({ status: 200, body: { accountId: "stale-b", revision: 2 } });
  });

  it("replaces dead state, including a credential from a different account", async () => {
    const user = "dead-import";
    const first = importedCredential("dead-account", { expiresInMs: 10 * 60_000 });
    expect((await importThroughControl(user, first)).status).toBe(204);

    const deadAt = first.expires_at - 4 * 60_000;
    vi.spyOn(Date, "now").mockReturnValue(deadAt);
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 401 }));
    expect(await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user)))
      .toEqual({ status: 422, body: { error: "chatgpt_credential_dead" } });

    const replacement = importedCredential("replacement-account", {
      expiresInMs: 60 * 60_000,
      marker: "replacement-secret",
    });
    expect((await importThroughControl(user, replacement)).status).toBe(204);
    expect(await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user)))
      .toMatchObject({
        status: 200,
        body: {
          secret: replacement.access_token,
          accountId: "replacement-account",
          revision: 1,
        },
      });
  });

  it("rolls back in-memory admission when the atomic encrypted commit fails", async () => {
    const user = "rollback-import";
    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    const imported = importedCredential("rollback-account");
    const failed = await runInDurableObject(
      stub,
      async (instance: UserCredentialBroker, state) => {
        const transaction = vi.spyOn(state.storage, "transaction")
          .mockRejectedValue(new Error("injected transaction failure"));
        try {
          const response = await instance.fetch(importRequest(imported));
          return { status: response.status, body: await response.json() };
        } finally {
          transaction.mockRestore();
        }
      },
    );
    expect(failed).toEqual({ status: 503, body: { error: "credential_broker_failed" } });
    expect(await internalCredential(stub))
      .toEqual({ status: 404, body: { error: "credential_not_configured" } });
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      expect(await state.storage.get("credential-state")).toBeUndefined();
      expect(await state.storage.getAlarm()).toBeNull();
    });
  });

  it("keeps local claim bodyless and hidden in production", async () => {
    const bodyful = await SELF.fetch(
      "https://broker.internal/users/bodyful-local/credentials/chatgpt/local-claim",
      { method: "POST", body: "provider-material" },
    );
    expect(bodyful.status).toBe(400);
    expect(await bodyful.json()).toEqual({ error: "invalid_request" });

    const production = await handleEgress(
      new Request(
        "https://broker.internal/users/hidden-local/credentials/chatgpt/local-claim",
        { method: "POST", body: "provider-material" },
      ),
      { ...workerEnv, ENVIRONMENT: "production" },
    );
    expect(production.status).toBe(404);
    expect(await production.json()).toEqual({ error: "not_found" });
  });

  it("marks local claims internally without exposing their provenance publicly", async () => {
    const user = "local-user-provenance";
    const claimed = await SELF.fetch(
      `https://broker.internal/users/${user}/credentials/chatgpt/local-claim`,
      { method: "POST" },
    );
    expect(claimed.status).toBe(200);
    expect(await claimed.json()).not.toHaveProperty("provenance");
    expect(await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user)))
      .toMatchObject({ status: 200, body: { provenance: "user" } });

    const sponsor = workerEnv.USER_CREDENTIALS.getByName("local-sponsor-provenance");
    const sponsored = await sponsor.fetch(
      "https://credentials.internal/v1/chatgpt/local-claim",
      {
        method: "POST",
        headers: { "x-nanocodex-credential-provenance": "sponsor" },
      },
    );
    expect(sponsored.status).toBe(200);
    expect(await sponsored.json()).not.toHaveProperty("provenance");
    expect(await internalCredential(sponsor))
      .toMatchObject({ status: 200, body: { provenance: "sponsor" } });
  });
});

function importedCredential(
  accountId: string,
  options: Readonly<{
    accessAccount?: string;
    accessFedramp?: boolean;
    expiresInMs?: number;
    marker?: string;
    refreshToken?: string;
  }> = {},
): ChatGptCredentialImport {
  const expiresAt = Math.ceil(
    (Date.now() + (options.expiresInMs ?? 60 * 60_000)) / 1_000,
  ) * 1_000;
  const fedramp = false;
  const accessFedramp = options.accessFedramp ?? fedramp;
  return {
    access_token: jwt({
      exp: expiresAt / 1_000,
      marker: options.marker ?? "access-secret",
      "https://api.openai.com/auth": {
        chatgpt_account_id: options.accessAccount ?? accountId,
        chatgpt_account_is_fedramp: accessFedramp,
      },
    }),
    refresh_token: options.refreshToken
      ?? `opaque-refresh::${options.marker ?? "refresh-secret"}+/=~`,
    account_id: accountId,
    expires_at: expiresAt,
    fedramp,
  };
}

function importThroughControl(user: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.internal/users/${user}/credentials/chatgpt`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function importRequest(body: ChatGptCredentialImport): Request {
  return new Request("https://credentials.internal/v1/chatgpt", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function internalCredential(
  broker: Pick<UserCredentialBroker, "fetch">,
): Promise<{ status: number; body: Record<string, unknown> }> {
  const response = await broker.fetch(new Request("https://credentials.internal/v1/credential", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ recover: false }),
  }));
  return { status: response.status, body: await response.json<Record<string, unknown>>() };
}

function jwt(payload: Record<string, unknown>): string {
  return `${base64Url({ alg: "none", typ: "JWT" })}.${base64Url(payload)}.signature`;
}

function base64Url(value: Record<string, unknown>): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
