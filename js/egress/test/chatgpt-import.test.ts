import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ChatGptCredentialImport, UserCredentialBroker } from "../src/broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const REFRESH_EARLY_MS = 5 * 60_000;

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("Service-Binding-only ChatGPT credential import", () => {
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

  it("rejects a healthy different account without reflecting either credential", async () => {
    const user = "conflicting-import";
    const first = importedCredential("first-account", { marker: "first-secret" });
    const conflicting = importedCredential("second-account", { marker: "second-secret" });
    expect((await importThroughControl(user, first)).status).toBe(204);

    const response = await importThroughControl(user, conflicting);
    expect(response.status).toBe(409);
    const body = await response.text();
    expect(JSON.parse(body)).toEqual({ error: "chatgpt_account_conflict" });
    expect(body).not.toMatch(/first-secret|second-secret|first-account|second-account/);
    expect(await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user)))
      .toMatchObject({ body: { secret: first.access_token, revision: 0 } });
  });

  it("serializes concurrent imports so exactly one different account wins", async () => {
    const user = "concurrent-import";
    const first = importedCredential("concurrent-a", { marker: "concurrent-secret-a" });
    const second = importedCredential("concurrent-b", { marker: "concurrent-secret-b" });
    const responses = await Promise.all([
      importThroughControl(user, first),
      importThroughControl(user, second),
    ]);
    expect(responses.map((response) => response.status).sort()).toEqual([204, 409]);
    for (const response of responses) {
      const body = await response.text();
      expect(body).not.toMatch(/concurrent-secret|concurrent-a|concurrent-b/);
    }
    const snapshot = await internalCredential(workerEnv.USER_CREDENTIALS.getByName(user));
    expect([first.access_token, second.access_token]).toContain(snapshot.body.secret);
    expect(snapshot.body.revision).toBe(0);
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
