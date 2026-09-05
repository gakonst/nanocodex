import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { ChatGptSubscription } from "nanocodex/browser";

import {
  CHATGPT_LOGIN_TTL_MS,
  CHATGPT_SESSION_TTL_MS,
  ChatGptSession,
} from "./subscriptionAuth.ts";

const TEST_KEY = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";

class MemoryStorage {
  readonly values = new Map<string, unknown>();
  alarm?: number;

  async get<T>(key: string): Promise<T | undefined> {
    return this.values.get(key) as T | undefined;
  }

  async put(key: string, value: unknown): Promise<void> {
    this.values.set(key, structuredClone(value));
  }

  async delete(key: string): Promise<boolean> {
    return this.values.delete(key);
  }

  async deleteAll(): Promise<void> {
    this.values.clear();
  }

  async setAlarm(timestamp: number): Promise<void> {
    this.alarm = timestamp;
  }

  async transaction<T>(callback: (transaction: MemoryStorage) => T | Promise<T>): Promise<T> {
    return callback(this);
  }
}

function jwt(payload: Record<string, unknown>): string {
  const encoded = Buffer.from(JSON.stringify(payload)).toString("base64url");
  return `header.${encoded}.signature`;
}

test("device login stores and rotates ChatGPT tokens without exposing them in public status", async () => {
  const originalFetch = globalThis.fetch;
  const originalNow = Date.now;
  let now = 1_800_000_000_000;
  Date.now = () => now;
  const requests: Array<{ url: string; init?: RequestInit }> = [];
  const accountClaims = {
    "https://api.openai.com/auth": {
      chatgpt_account_id: "account-1",
      chatgpt_account_is_fedramp: true,
    },
  };
  globalThis.fetch = (async function (this: typeof globalThis, input: string | URL | Request, init?: RequestInit) {
    assert.equal(this, globalThis, "the subscription bridge preserves the Worker fetch receiver");
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    requests.push({ url, init });
    if (url.endsWith("/api/accounts/deviceauth/usercode")) {
      return Response.json({ device_auth_id: "device-1", user_code: "ABCD-EFGH", interval: 1 });
    }
    if (url.endsWith("/api/accounts/deviceauth/token")) {
      return Response.json({
        authorization_code: "authorization-1",
        code_verifier: "verifier-1",
        code_challenge: "challenge-1",
      });
    }
    if (url.endsWith("/oauth/token") && new Headers(init?.headers).get("content-type") === "application/x-www-form-urlencoded") {
      return Response.json({
        access_token: jwt({ exp: Math.floor((now + 3_600_000) / 1_000) }),
        refresh_token: "refresh-1",
        id_token: jwt(accountClaims),
      });
    }
    if (url.endsWith("/oauth/token")) {
      return Response.json({
        access_token: jwt({ exp: Math.floor((now + 7_200_000) / 1_000) }),
        refresh_token: "refresh-2",
      });
    }
    throw new Error(`unexpected URL ${url}`);
  }) as typeof fetch;

  try {
    const storage = new MemoryStorage();
    const state = {
      id: { toString: () => "session-id" },
      storage,
    } as unknown as DurableObjectState;
    const module = await WebAssembly.compile(await readFile(new URL(
      "../../nanocodex/pkg-web/nanocodex_bg.wasm",
      import.meta.url,
    )));
    const session = new ChatGptSession(state, {
      CHATGPT_ISSUER: "http://127.0.0.1:8799/",
      ENVIRONMENT: "test",
      SESSION_CREDENTIAL_KEY: TEST_KEY,
    }, {
      open: (options) => ChatGptSubscription.open({ ...options, module }),
    });

    const started = await session.fetch(new Request("https://session.test/start", { method: "POST" }));
    assert.deepEqual(await started.json(), {
      state: "pending",
      verificationUrl: "http://127.0.0.1:8799/codex/device",
      userCode: "ABCD-EFGH",
      expiresAt: now + 900_000,
      pollAfterMs: 1_000,
    });
    assert.equal(requests.length, 1);
    assert.equal(storage.alarm, now + CHATGPT_LOGIN_TTL_MS);

    const pending = await session.fetch(new Request("https://session.test/status"));
    assert.equal((await pending.json() as { state: string }).state, "pending");
    assert.equal(requests.length, 1, "status respects the issuer polling interval");

    now += 1_001;
    const authenticated = await session.fetch(new Request("https://session.test/status"));
    const publicStatus = await authenticated.json() as Record<string, unknown>;
    assert.equal(publicStatus.state, "authenticated");
    assert.equal(publicStatus.accountId, "account-1");
    assert.equal("accessToken" in publicStatus, false);
    assert.equal("refreshToken" in publicStatus, false);

    assert.equal(storage.alarm, now + CHATGPT_SESSION_TTL_MS);

    const internal = await session.fetch(new Request("https://session.test/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation: "health" }),
    }));
    const credential = await internal.json() as Record<string, unknown>;
    assert.equal(credential.kind, "chatgpt");
    assert.equal(credential.accountId, "account-1");
    assert.equal(credential.fedramp, true);
    assert.equal(credential.revision, "0");
    assert.equal("refreshToken" in credential, false);

    for (let requestIndex = 0; requestIndex < 4; requestIndex += 1) {
      const allowed = await session.fetch(new Request("https://session.test/credential", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ operation: "image" }),
      }));
      assert.equal(allowed.status, 200);
    }
    const imageLimited = await session.fetch(new Request("https://session.test/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation: "image" }),
    }));
    assert.equal(imageLimited.status, 429);
    assert.equal(imageLimited.headers.get("retry-after"), "60");

    for (let socketIndex = 0; socketIndex < 8; socketIndex += 1) {
      const allowed = await session.fetch(new Request("https://session.test/credential", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ operation: "socket", leaseId: String(socketIndex).repeat(43) }),
      }));
      assert.equal(allowed.status, 200);
    }
    const socketLimited = await session.fetch(new Request("https://session.test/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation: "socket", leaseId: "z".repeat(43) }),
    }));
    assert.equal(socketLimited.status, 429);
    await session.fetch(new Request("https://session.test/lease", {
      method: "DELETE",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ leaseId: "0".repeat(43) }),
    }));
    const socketAfterRelease = await session.fetch(new Request("https://session.test/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation: "socket", leaseId: "z".repeat(43) }),
    }));
    assert.equal(socketAfterRelease.status, 200);

    const recovered = await session.fetch(new Request("https://session.test/recover", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ revision: "0" }),
    }));
    const rotated = await recovered.json() as Record<string, unknown>;
    assert.equal(rotated.revision, "1");
    assert.equal(rotated.accountId, "account-1");
    const stored = JSON.stringify(storage.values.get("subscription"));
    assert.doesNotMatch(stored, /refresh-2|refresh-1|account-1/);
  } finally {
    globalThis.fetch = originalFetch;
    Date.now = originalNow;
  }
});
