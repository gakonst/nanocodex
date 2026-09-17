import { env, runInDurableObject } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MemoryScope } from "../src/memory-scope";
import { PhoneContainer, validPhoneCheckpoint, type PhoneContainerEnv } from "../src/phone-container";

// Exercise real DO SQLite while replacing only the native container lifecycle.
vi.mock("@cloudflare/containers", async importOriginal => ({ ...await importOriginal<typeof import("@cloudflare/containers")>(), Container: class {
  constructor(public ctx: DurableObjectState, public env: PhoneContainerEnv) {}
  startAndWaitForPorts = vi.fn().mockResolvedValue(undefined);
  containerFetch = vi.fn().mockResolvedValue(new Response("native"));
} }));
const binding = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
const token = "bridge-token-".repeat(4);
const config: PhoneContainerEnv = {
  NANOCODEX_PHONE_BRIDGE_TOKEN: token, NANOCODEX_PHONE_OWNER_ID: "owner", NANOCODEX_PHONE_ADMIN_ID: "owner",
  NANOCODEX_PHONE_MANAGED_API_KEY: "managed-key", TWILIO_ACCOUNT_SID: `AC${"a".repeat(32)}`,
  TWILIO_API_KEY_SID: `SK${"b".repeat(32)}`, TWILIO_API_KEY_SECRET: "api-secret", TWILIO_VOICE_FROM_NUMBER: "+15551234567",
};
const row = () => {
  const id = crypto.randomUUID();
  return { id, agent: crypto.randomUUID(), operation: crypto.randomUUID(), fingerprint: "a".repeat(64),
    record: { call_id: id, status: "preparing", transcript: [] as { speaker: string; text: string }[], max_duration_seconds: 180 } };
};
const request = (path: string, body?: unknown, authorization = `Bearer ${token}`) => new Request(`https://phone.internal${path}`, {
  method: body === undefined ? "GET" : "POST", headers: { authorization }, ...(body === undefined ? {} : { body: JSON.stringify(body && typeof body === "object" && "record" in body ? { ...body, record: typeof body.record === "string" ? body.record : JSON.stringify(body.record) } : body) }),
});
afterEach(() => vi.unstubAllGlobals());
async function withPhone(test: (phone: PhoneContainer, state: DurableObjectState) => Promise<void>, overrides: Partial<PhoneContainerEnv> = {}) {
  await runInDurableObject(binding.getByName(crypto.randomUUID()), async (_memory, state) => test(new PhoneContainer(state, { ...config, ...overrides }), state));
}

describe("phone container", () => {
  it("rejects internal access without the bridge credential before network or startup", async () => {
    const network = vi.fn(); vi.stubGlobal("fetch", network);
    await withPhone(async phone => {
      for (const path of ["/internal/state", "/internal/setup"]) expect((await phone.fetch(request(path, undefined, "Bearer wrong"))).status).toBe(401);
      expect(network).not.toHaveBeenCalled();
      expect(phone.startAndWaitForPorts).not.toHaveBeenCalled();
    });
  });
  it("persists hydration rows across object reconstruction and enforces immutable idempotency identity", async () => {
    await withPhone(async (phone, state) => {
      const original = row();
      expect((await phone.fetch(request("/internal/state", original))).status).toBe(200);
      original.record.status = "in-progress";
      original.record.transcript.push({ speaker: "assistant", text: "Hello" });
      expect((await phone.fetch(request("/internal/state", original))).status).toBe(200);
      for (const changed of [{ ...original, fingerprint: "b".repeat(64) }, { ...original, agent: crypto.randomUUID() }, { ...original, id: crypto.randomUUID(), record: { ...original.record } }]) {
        changed.record = { ...changed.record, call_id: changed.id };
        expect((await phone.fetch(request("/internal/state", changed))).status).toBe(409);
      }
      const fresh = new PhoneContainer(state, config);
      expect(await (await fresh.fetch(request("/internal/state"))).json()).toEqual({ calls: [{ ...original, record: JSON.stringify(original.record) }] });
      expect(phone.startAndWaitForPorts).not.toHaveBeenCalled();
      expect(fresh.startAndWaitForPorts).not.toHaveBeenCalled();
    });
  });
  it("rejects arbitrary persisted fields, malformed transcripts and oversized bodies", async () => {
    await withPhone(async phone => {
      const original = row();
      expect(validPhoneCheckpoint({ ...original, record: JSON.stringify(original.record) })).toBe(true);
      for (const record of [{ ...original.record, TWILIO_AUTH_TOKEN: "secret" }, { ...original.record, transcript: [{ speaker: "system", text: "bad" }] }]) {
        expect((await phone.fetch(request("/internal/state", { ...original, record }))).status).toBe(400);
      }
      expect((await phone.fetch(request("/internal/state", { text: "a".repeat(1024 * 1024) }))).status).toBe(413);
      expect(await (await phone.fetch(request("/internal/state"))).json()).toEqual({ calls: [] });
    });
  });
  it("returns sanitized setup metadata, keeps discovered auth in memory, and forwards the original response", async () => {
    const auth = "c".repeat(32);
    const network = vi.fn().mockImplementation(async (url: string, init: RequestInit) => {
      const request = new Request(url, init);
      expect(request.redirect).toBe("manual");
      return Response.json(url.includes("IncomingPhoneNumbers")
      ? { incoming_phone_numbers: [{ phone_number: "+15551234567", capabilities: { voice: true }, auth_token: auth }, { phone_number: "+15557654321", capabilities: { voice: false } }] }
      : { sid: config.TWILIO_ACCOUNT_SID, auth_token: auth }); });
    vi.stubGlobal("fetch", network);
    await withPhone(async (phone, state) => {
      expect(await (await phone.fetch(request("/internal/setup"))).json()).toEqual({ phone_numbers: ["+15551234567"], verified_caller_ids: [], caller_id_lookup_status: 200, number_lookup_available: true, webhook_auth_available: true, number_lookup_status: 200, auth_lookup_status: 200, auth_token_metadata: { account_matches: true, token_returned: true, token_usable: true }, account_sid_valid: true, api_key_configured: true });
      expect(phone.startAndWaitForPorts).not.toHaveBeenCalled();
      const raw = request("/calls?agent_id=test");
      const native = new Response("native-response");
      vi.mocked(phone.containerFetch).mockResolvedValue(native);
      expect(await phone.fetch(raw)).toBe(native);
      expect(phone.containerFetch).toHaveBeenCalledWith(raw, 8788);
      expect(phone.startAndWaitForPorts).toHaveBeenCalledWith({ ports: [8788], startOptions: { enableInternet: true, envVars: expect.objectContaining({
        TWILIO_AUTH_TOKEN: auth, NANOCODEX_PHONE_PUBLIC_PREFIX: "/v1/phone/bridge", NANOCODEX_PHONE_PUBLIC_ORIGIN: "https://nanocodex.gakonst.workers.dev", NANOCODEX_PHONE_HOST: "0.0.0.0",
      }) } });
      expect(network.mock.calls[0][1]).toMatchObject({ redirect: "manual", headers: { Authorization: `Basic ${btoa(`${config.TWILIO_API_KEY_SID}:${config.TWILIO_API_KEY_SECRET}`)}` } });
      expect(state.storage.sql.exec("SELECT * FROM phone_calls").toArray()).toEqual([]);
      expect(await state.storage.list()).toEqual(new Map());
    });
  });
  it("fails closed when Twilio refuses account-token discovery", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("secret provider diagnostic", { status: 403 })));
    await withPhone(async phone => {
      expect(await (await phone.fetch(request("/internal/setup"))).json()).toEqual({ phone_numbers: [], verified_caller_ids: [], caller_id_lookup_status: 403, number_lookup_available: false, webhook_auth_available: false, number_lookup_status: 403, auth_lookup_status: 403, auth_token_metadata: { account_matches: false, token_returned: false, token_usable: false }, account_sid_valid: true, api_key_configured: true });
      const response = await phone.fetch(request("/calls"));
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({ error: "phone_not_configured" });
      expect(phone.startAndWaitForPorts).not.toHaveBeenCalled();
    });
  });
  it("allows hydration callback while native readiness is pending", async () => {
    await withPhone(async phone => {
      vi.mocked(phone.startAndWaitForPorts).mockImplementation(async () => {
        expect(await (await phone.fetch(request("/internal/state"))).json()).toEqual({ calls: [] });
      });
      expect((await phone.fetch(request("/health"))).status).toBe(200);
    }, { TWILIO_AUTH_TOKEN: "c".repeat(32) });
  });
});

it("retains only a distinct valid delegated thread and paired voice-session identity", () => {
  const value = row();
  const record = { ...value.record, delegate_agent_id: crypto.randomUUID(), delegate_session_id: crypto.randomUUID(), delegate_cleaned: false };
  const valid = (r: unknown) => validPhoneCheckpoint({ ...value, record: JSON.stringify(r) });
  expect(valid(record)).toBe(true);
  expect(valid({ ...record, delegate_agent_id: value.agent })).toBe(false);
  expect(valid({ ...record, delegate_session_id: "../other" })).toBe(false);
  const { delegate_session_id: _session, ...unpaired } = record;
  expect(valid(unpaired)).toBe(false);
});

it.each([undefined, "", "other"])("blocks all container routes before provider access without the deployment phone admin: %s", async admin => {
  const network = vi.fn();
  vi.stubGlobal("fetch", network);
  await withPhone(async phone => {
    for (const path of ["/health", "/calls", "/internal/setup", "/internal/state", `/status/${crypto.randomUUID()}`, `/media/${crypto.randomUUID()}/`]) {
      expect((await phone.fetch(request(path))).status).toBe(503);
    }
    expect(phone.startAndWaitForPorts).not.toHaveBeenCalled();
    expect(phone.containerFetch).not.toHaveBeenCalled();
    expect(network).not.toHaveBeenCalled();
  }, { NANOCODEX_PHONE_ADMIN_ID: admin });
});
