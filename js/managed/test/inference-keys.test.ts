import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { Principal } from "../src/account-auth";
import {
  authorizeInferenceKey, DEFAULT_INFERENCE_KEY_LIMITS, routeInferenceKeys,
  type InferenceKeyMetadata, type InferenceKeysEnv,
} from "../src/inference-keys";

const bindings = env as unknown as InferenceKeysEnv;
const origin = "https://inference.example";
function principal(): Principal {
  const userId = crypto.randomUUID();
  return {
    kind: "account_session", userId, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: `user:${userId}`, credentialId: "test-session", authorizationEpoch: 1,
    capabilities: ["api_keys:read", "api_keys:write"],
  };
}
function req(path: string, method = "GET", body?: unknown, headers: HeadersInit = {}): Request {
  return new Request(`${origin}${path}`, {
    method, headers: { origin, "content-type": "application/json", ...headers },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}
async function route(owner: Principal | null, method: string, body?: unknown, id?: string, headers: HeadersInit = {}, e = bindings) {
  const request = req(`/v1/inference/keys${id ? `/${id}` : ""}`, method, body, headers);
  return (await routeInferenceKeys(request, e, new URL(request.url), owner))!;
}
async function create(owner = principal(), body: Record<string, unknown> = {}) {
  const result = await route(owner, "POST", body);
  expect(result.status).toBe(201);
  return { ...await result.json<{ api_key: string; key: InferenceKeyMetadata }>(), owner };
}
function auth(token: string, reserve = false) {
  return authorizeInferenceKey(req("/v1/inference/responses", "POST", {}, { authorization: `Bearer ${token}` }), bindings, { reserve });
}
async function status(result: ReturnType<typeof auth>) {
  const value = await result;
  return value instanceof Response ? value.status : 200;
}

// These tests execute actual SQLite Durable Objects in workerd, including concurrent fetches.
describe("standalone inference key authority", () => {
  it("issues a separate token and stores only SHA256 in its key object and metadata in its registry", async () => {
    const { owner, api_key, key } = await create();
    expect(api_key).toMatch(/^nci_live_[A-Za-z0-9_-]{43}_[A-Za-z0-9_-]{43}$/);
    expect(api_key).not.toMatch(/^ncx_live_/);
    expect(key.scope).toBe("inference");
    expect(key.limits).toEqual(DEFAULT_INFERENCE_KEY_LIMITS);
    expect(key.expiresAt).toBe(key.createdAt + 30 * 86_400_000);
    const expected = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(api_key)))]
      .map(byte => byte.toString(16).padStart(2, "0")).join("");
    await runInDurableObject(bindings.NANOCODEX_INFERENCE_KEYS.getByName(key.id), async (_, state) => {
      const record = await state.storage.get<Record<string, unknown>>("record");
      expect(record?.digest).toBe(expected);
      expect(record?.userId).toBe(owner.userId);
      expect(record?.scope).toBe("inference");
      expect(JSON.stringify([...await state.storage.list()])).not.toContain(api_key);
    });
    await runInDurableObject(bindings.NANOCODEX_INFERENCE_ACCOUNTS.getByName(owner.userId), async (_, state) => {
      const stored = JSON.stringify([...await state.storage.list()]);
      expect(stored).not.toContain(expected);
      expect(stored).not.toContain(api_key);
      expect(stored).not.toContain("digest");
    });
    expect(await auth(api_key)).toEqual({ kind: "inference_key", scope: "inference", id: key.id, userId: owner.userId, limits: key.limits });
    const listed = await route(owner, "GET");
    expect(await listed.json()).toEqual({ data: [key] });
    expect(listed.headers.get("cache-control")).toBe("no-store");
    const read = await bindings.NANOCODEX_INFERENCE_KEYS.getByName(key.id).fetch(req("/record"));
    expect(await read.json()).toEqual(key);
  });

  it("requires immutable inference scope in private records and denies missing or altered stored scope", async () => {
    const { owner, api_key, key } = await create();
    for (const scope of [undefined, "account", "connectors", "hands"]) {
      const stub = bindings.NANOCODEX_INFERENCE_KEYS.getByName(crypto.randomUUID());
      expect((await stub.fetch(req("/record", "PUT", {
        ...key, scope, userId: owner.userId, digest: "a".repeat(64),
      }))).status).toBe(400);
      const registry = bindings.NANOCODEX_INFERENCE_ACCOUNTS.getByName(crypto.randomUUID());
      expect((await registry.fetch(req("/keys", "POST", {
        key: { ...key, scope }, operation_id: crypto.randomUUID(),
      }))).status).toBe(400);
      await runInDurableObject(bindings.NANOCODEX_INFERENCE_KEYS.getByName(key.id), async (_, state) => {
        const record = await state.storage.get<Record<string, unknown>>("record");
        await state.storage.put("record", { ...record, scope });
      });
      expect(await status(auth(api_key))).toBe(401);
      expect(await status(auth(api_key, true))).toBe(401);
    }
  });

  it("rejects incorrect secrets, account tokens, query/cookie credentials and nonexact bearer headers", async () => {
    const { api_key } = await create();
    const forged = api_key.slice(0, -1) + (api_key.endsWith("a") ? "b" : "a");
    expect(await status(auth(forged))).toBe(401);
    for (const authorization of [api_key, `bearer ${api_key}`, `Bearer  ${api_key}`, `Bearer ${api_key},other`, "Bearer ncx_live_foo"]) {
      const result = await authorizeInferenceKey(req("/v1/inference/responses", "GET", undefined, { authorization }), bindings);
      expect(result).toBeInstanceOf(Response);
      expect((result as Response).status).toBe(401);
      expect(await (result as Response).text()).not.toContain(api_key);
    }
    const noBearer = await authorizeInferenceKey(req(`/v1/inference/responses?api_key=${api_key}`, "GET", undefined, { cookie: `nanocodex_account=${api_key}` }), bindings);
    expect((noBearer as Response).status).toBe(401);
  });

  it("rechecks expiry and revocation on both authorize and reserve, with no resurrection", async () => {
    const expired = await create();
    await runInDurableObject(bindings.NANOCODEX_INFERENCE_KEYS.getByName(expired.key.id), async (_, state) => {
      const record = await state.storage.get<Record<string, unknown>>("record");
      await state.storage.put("record", { ...record, expiresAt: Date.now() - 1 });
    });
    expect(await status(auth(expired.api_key))).toBe(401);
    expect(await status(auth(expired.api_key, true))).toBe(401);
    const live = await create();
    expect((await route(live.owner, "DELETE", undefined, live.key.id)).status).toBe(204);
    expect(await status(auth(live.api_key))).toBe(401);
    expect(await status(auth(live.api_key, true))).toBe(401);
    const stub = bindings.NANOCODEX_INFERENCE_KEYS.getByName(live.key.id);
    expect((await stub.fetch(req("/record", "PUT", { ...live.key, digest: "a".repeat(64), userId: live.owner.userId }))).status).toBe(409);
    const listed = await (await route(live.owner, "GET")).json<{ data: InferenceKeyMetadata[] }>();
    expect(listed.data[0]?.revokedAt).toBeTypeOf("number");
    expect((await route(live.owner, "DELETE", undefined, live.key.id)).status).toBe(204);
  });

  it("protects another owner's keys and refuses to create over an early revocation tombstone", async () => {
    const { owner, api_key, key } = await create();
    expect((await route(principal(), "DELETE", undefined, key.id)).status).toBe(404);
    expect(await status(auth(api_key))).toBe(200);
    const stub = bindings.NANOCODEX_INFERENCE_KEYS.getByName("z".repeat(43));
    expect((await stub.fetch(req("/record", "DELETE"))).status).toBe(204);
    expect((await stub.fetch(req("/record", "PUT", { ...key, id: "z".repeat(43), userId: owner.userId, digest: "a".repeat(64) }))).status).toBe(409);
  });

  it("reserves minute/day slots atomically and resets elapsed windows without charging authorization", async () => {
    const { api_key, key } = await create(principal(), { limits: { requestsPerMinute: 3, requestsPerDay: 5 } });
    const authorized = await Promise.all(Array.from({ length: 12 }, () => status(auth(api_key))));
    expect(authorized.every(code => code === 200)).toBe(true);
    const attempts = await Promise.all(Array.from({ length: 12 }, () => status(auth(api_key, true))));
    expect(attempts.filter(code => code === 200)).toHaveLength(3);
    expect(attempts.filter(code => code === 429)).toHaveLength(9);
    const limited = await auth(api_key, true) as Response;
    expect(limited.headers.get("retry-after")).toMatch(/^\d+$/);
    const stub = bindings.NANOCODEX_INFERENCE_KEYS.getByName(key.id);
    await runInDurableObject(stub, async (_, state) => {
      const usage = (await state.storage.get<Record<string, number>>("usage"))!;
      expect(usage.dayCount).toBe(3);
      await state.storage.put("usage", { ...usage, minute: usage.minute! - 1 });
    });
    expect(await status(auth(api_key, true))).toBe(200);
    expect(await status(auth(api_key, true))).toBe(200);
    expect(await status(auth(api_key, true))).toBe(429);
    await runInDurableObject(stub, async (_, state) => {
      const usage = (await state.storage.get<Record<string, number>>("usage"))!;
      expect(usage.dayCount).toBe(5);
      await state.storage.put("usage", { ...usage, day: usage.day! - 1, minute: usage.minute! - 1 });
    });
    expect(await status(auth(api_key, true))).toBe(200);
  });

  it("requires full account key scopes, rejects Connect/service/inference principals and ambient-cookie nci", async () => {
    const owner = principal();
    expect((await route(null, "GET")).status).toBe(401);
    for (const kind of ["connect_grant", "service", "inference_key"]) {
      expect((await route({ ...owner, kind } as Principal, "POST", {})).status).toBe(403);
    }
    expect((await route({ ...owner, connectGrant: { grantId: "synthetic", connectors: [], mcpIds: [] } }, "GET")).status).toBe(403);
    expect((await route({ ...owner, capabilities: [] }, "GET")).status).toBe(403);
    expect((await route({ ...owner, capabilities: ["api_keys:read"] }, "POST", {})).status).toBe(403);
    expect((await route(owner, "POST", {}, undefined, { origin: "https://evil.example" })).status).toBe(403);
    for (const authorization of ["Bearer nci_live_invalid", "bearer NCI_live_invalid"]) {
      expect((await route(owner, "POST", {}, undefined, { authorization, cookie: "nanocodex_account=ambient" })).status).toBe(403);
    }
    expect((await route({ ...owner, kind: "api_key" }, "POST", {}, undefined, { origin: "" })).status).toBe(201);
    expect((await route(owner, "PATCH", {})).status).toBe(405);
  });

  it("claims an issuance UUID only once, including concurrent retries, and never repeats the secret", async () => {
    const owner = principal();
    const operation_id = crypto.randomUUID();
    const responses = await Promise.all(Array.from({ length: 8 }, () => route(owner, "POST", { label: "Teammate", operation_id })));
    expect(responses.filter(response => response.status === 201)).toHaveLength(1);
    expect(responses.filter(response => response.status === 409)).toHaveLength(7);
    const issued = await responses.find(response => response.status === 201)!.json<{ api_key: string; key: InferenceKeyMetadata }>();
    for (const response of responses.filter(response => response.status === 409)) {
      expect(await response.json()).toEqual({ error: "already_issued", key: issued.key });
    }
    const repeat = await route(owner, "POST", { operation_id: operation_id.toUpperCase(), label: "Different" });
    expect(repeat.status).toBe(409);
    expect(await repeat.json()).toEqual({ error: "already_issued", key: issued.key });
    expect((await (await route(owner, "GET")).json<{ data: unknown[] }>()).data).toHaveLength(1);
    // The same client operation ID belongs to its owner, not a global namespace.
    expect((await route(principal(), "POST", { operation_id })).status).toBe(201);
  });

  it("does not retry an ambiguous key write or issue a replacement on operation replay", async () => {
    const owner = principal();
    const operation_id = crypto.randomUUID();
    let writes = 0;
    const unavailable = {
      ...bindings,
      NANOCODEX_INFERENCE_ACCOUNTS: bindings.NANOCODEX_INFERENCE_ACCOUNTS,
      NANOCODEX_INFERENCE_KEYS: { getByName: () => ({ fetch: () => { writes++; throw new Error("private backend details"); } }) },
    } as unknown as InferenceKeysEnv;
    const failed = await route(owner, "POST", { operation_id }, undefined, {}, unavailable);
    expect(failed.status).toBe(503);
    expect(await failed.text()).not.toContain("private backend details");
    expect(writes).toBe(1);
    const replay = await route(owner, "POST", { operation_id });
    expect(replay.status).toBe(409);
    const value = await replay.json<Record<string, unknown>>();
    expect(value.error).toBe("already_issued");
    expect(value).not.toHaveProperty("api_key");
  });

  it("validates issuance options before persisting records", async () => {
    const owner = principal();
    for (const body of [
      { expires_at: Date.now() - 1 }, { expires_at: null }, { label: " " }, { operation_id: "wrong" },
      { limits: { requestsPerDay: 0 } }, { limits: { maxOutputTokens: 0 } }, { limits: { maxOutputTokens: 4097 } },
      { limits: { requestsPerMinute: 1.5 } }, { limits: { unknown: 10 } }, { limits: null },
      { connectors: ["github"] }, { userId: crypto.randomUUID() },
      { scope: "inference" }, { scope: "account" }, { capabilities: ["tools:use"] },
    ]) expect((await route(owner, "POST", body)).status).toBe(400);
    expect(await (await route(owner, "GET")).json()).toEqual({ data: [] });
    expect((await route(owner, "POST", { label: "x".repeat(5000) })).status).toBe(413);
  });
});
