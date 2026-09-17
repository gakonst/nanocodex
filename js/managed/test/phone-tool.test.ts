import { phoneAdminConfigured } from "../src/phone-admin";
import { afterEach, expect, it, vi } from "vitest";
import { phoneTools, type PhoneConfig } from "../src/phone-tool";

const id = "11111111-1111-4111-8111-111111111111";
const token = "secret-bridge-token".repeat(2);
const snapshot = { call_id: id, status: "in-progress", transcript: [{ speaker: "user", text: "Hello" }], max_duration_seconds: 180 };
const context = () => ({ callId: "call", parentCallId: "", sessionId: "session", model: "test", signal: new AbortController().signal });
const input = { operation: "call", to: "+14155550123", instructions: "Ask for opening hours.", operation_id: id };
const configuration = (): PhoneConfig => ({ NANOCODEX_PHONE_BRIDGE_URL: "https://phone.example", NANOCODEX_PHONE_BRIDGE_TOKEN: token, NANOCODEX_PHONE_OWNER_ID: "owner", NANOCODEX_PHONE_ADMIN_ID: "owner" });
function fixture() {
  const config = configuration();
  const authorize = vi.fn();
  const fetcher = vi.fn().mockResolvedValue(Response.json({ ...snapshot, secret: token }));
  vi.stubGlobal("fetch", fetcher);
  const tool = phoneTools({ config, owner: "owner", agentId: "agent/id", authorize })[0]!;
  return { config, authorize, fetcher, tool };
}
afterEach(() => vi.unstubAllGlobals());

it("is disabled without complete configuration, for other owners, and in multiplayer", () => {
  for (const config of [{}, { ...configuration(), NANOCODEX_PHONE_BRIDGE_TOKEN: "" }, { ...configuration(), NANOCODEX_PHONE_OWNER_ID: "other" }])
    expect(phoneTools({ config, owner: "owner", agentId: id, authorize() {} })).toEqual([]);
  expect(phoneTools({ config: configuration(), owner: "owner", agentId: id, multiplayer: true, authorize() {} })).toEqual([]);
});
it.each(["http://phone.example", "https://user:pass@phone.example", "https://phone.example/path", "https://phone.example?x=1", "https://phone.example#x", "invalid", " https://phone.example"])("rejects non-origin configuration %s", url => {
  expect(phoneTools({ config: { ...configuration(), NANOCODEX_PHONE_BRIDGE_URL: url }, owner: "owner", agentId: id, authorize() {} })).toEqual([]);
});
it("checks current authority and configured owner on every invocation before fetch", async () => {
  const f = fixture();
  await f.tool.handler(input, context());
  f.authorize.mockImplementation(() => { throw new Error("forbidden"); });
  await expect(f.tool.handler({ operation: "status", call_id: id }, context())).rejects.toThrow("forbidden");
  f.authorize.mockReset();
  f.config.NANOCODEX_PHONE_OWNER_ID = "other";
  await expect(f.tool.handler({ operation: "hangup", call_id: id }, context())).rejects.toThrow("unavailable");
  expect(f.authorize).toHaveBeenCalledOnce();
  expect(f.fetcher).toHaveBeenCalledOnce();
});
it("sends scoped call requests with stable operation IDs and projects safe results", async () => {
  const f = fixture();
  expect(await f.tool.handler(input, context())).toEqual(snapshot);
  const [url, init] = f.fetcher.mock.calls[0]!;
  expect(url).toBe("https://phone.example/calls");
  expect(init).toMatchObject({ method: "POST", redirect: "manual", headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" } });
  expect(JSON.parse(init.body)).toEqual({ agent_id: "agent/id", operation_id: id, to: input.to, instructions: input.instructions, max_duration_seconds: 180 });
  expect(init.signal).toBeInstanceOf(AbortSignal);
});
it("sends agent-scoped status and hangup requests", async () => {
  const f = fixture();
  await f.tool.handler({ operation: "status", call_id: id }, context());
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, status: "completed" }));
  await f.tool.handler({ operation: "hangup", call_id: id }, context());
  expect(f.fetcher.mock.calls[0]).toMatchObject([`https://phone.example/calls/${id}?agent_id=agent%2Fid`, { method: "GET" }]);
  expect(f.fetcher.mock.calls[0]![1].body).toBeUndefined();
  expect(f.fetcher.mock.calls[1]).toMatchObject([`https://phone.example/calls/${id}/hangup`, { method: "POST", body: JSON.stringify({ agent_id: "agent/id" }) }]);
});
it.each([
  { ...input, to: "4155550123" }, { ...input, instructions: " " }, { ...input, instructions: "a".repeat(8001) },
  { ...input, operation_id: undefined }, { ...input, operation_id: "bad" }, { ...input, max_duration_seconds: 29 },
  { ...input, max_duration_seconds: null }, { ...input, max_duration_seconds: 601 }, { ...input, max_duration_seconds: 30.1 }, { ...input, call_id: id },
  { operation: "status", call_id: "../secret" }, { operation: "hangup", call_id: id, to: input.to }, { operation: "unknown" }, null,
])("validates operation arguments before network access", async value => {
  const f = fixture();
  await expect(f.tool.handler(value, context())).rejects.toThrow();
  expect(f.fetcher).not.toHaveBeenCalled();
});
it("sanitizes transport and provider errors and never retries writes", async () => {
  for (const response of [new Error(token + " private URL"), new Response(token, { status: 500 }), new Response(token), Response.json({ credentials: token })]) {
    const f = fixture();
    if (response instanceof Error) f.fetcher.mockRejectedValue(response); else f.fetcher.mockResolvedValue(response);
    await expect(f.tool.handler(input, context())).rejects.toThrow("outcome may be unknown");
    expect(f.fetcher).toHaveBeenCalledOnce();
  }
  const f = fixture();
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, transcript: [{ speaker: "user", text: `hello ${token}` }], access_token: token }));
  expect(JSON.stringify(await f.tool.handler(input, context()))).not.toContain(token);
});
it("honors cancellation before and during transport and uses a 45 second timeout", async () => {
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  await expect(f.tool.handler(input, { ...context(), signal: controller.signal })).rejects.toThrow();
  expect(f.fetcher).not.toHaveBeenCalled();
  const timeout = vi.spyOn(AbortSignal, "timeout");
  const active = new AbortController();
  f.fetcher.mockImplementation(async (_url, init) => {
    active.abort();
    expect(init.signal.aborted).toBe(true);
    throw new Error(token);
  });
  await expect(f.tool.handler(input, { ...context(), signal: active.signal })).rejects.toThrow("interrupted");
  expect(timeout).toHaveBeenCalledWith(45_000);
  timeout.mockRestore();
});

it.each([30, 600])("accepts duration boundary %s", async max_duration_seconds => {
  const f = fixture();
  await f.tool.handler({ ...input, max_duration_seconds, instructions: "a".repeat(8000) }, context());
  expect(JSON.parse(f.fetcher.mock.calls[0]![1].body).max_duration_seconds).toBe(max_duration_seconds);
});

it("projects polling transcript entries and preserves uncertain call states without provider fields", async () => {
  const f = fixture();
  for (const status of ["preparing", "unknown"]) {
    f.fetcher.mockResolvedValue(Response.json({ call_id: id, status, transcript: [
      { speaker: "user", text: "Hello " + token, secret: token },
      { speaker: "assistant", text: "Opening hours?" },
    ], max_duration_seconds: 180, error: token }));
    expect(await f.tool.handler({ operation: "status", call_id: id }, context())).toEqual({
      call_id: id, status, max_duration_seconds: 180, transcript: [
        { speaker: "user", text: "Hello [redacted]" },
        { speaker: "assistant", text: "Opening hours?" },
      ],
    });
  }
  expect(f.fetcher).toHaveBeenCalledTimes(2);
});

it.each(["x".repeat(31), "x".repeat(32) + " ", "x".repeat(32) + "\n"])("disables invalid bridge tokens", token => {
  expect(phoneTools({ config: { ...configuration(), NANOCODEX_PHONE_BRIDGE_TOKEN: token }, owner: "owner", agentId: id, authorize() {} })).toEqual([]);
});
it.each(["01111111-1111-0111-8111-111111111111", "11111111-1111-4111-7111-111111111111"])("rejects unsupported UUID versions/variants", async operation_id => {
  const f = fixture();
  await expect(f.tool.handler({ ...input, operation_id }, context())).rejects.toThrow();
  expect(f.fetcher).not.toHaveBeenCalled();
});
it.each([
  { call_id: undefined }, { call_id: "11111111-1111-9111-8111-111111111111" }, { status: "secret" },
  { max_duration_seconds: 29 }, { max_duration_seconds: 601 }, { max_duration_seconds: 180.1 }, { max_duration_seconds: undefined },
  { transcript: "hello" }, { transcript: Array.from({ length: 201 }, () => ({ speaker: "user", text: "hello" })) },
  { transcript: [{ speaker: "system", text: "secret" }] }, { transcript: [{ speaker: "user", text: "x".repeat(4001) }] },
])("rejects malformed bridge snapshots without retries", async patch => {
  const f = fixture();
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, ...patch }));
  await expect(f.tool.handler(input, context())).rejects.toThrow("outcome may be unknown");
  expect(f.fetcher).toHaveBeenCalledOnce();
});
it.each(["call_start_failed_or_unknown", "hangup_unconfirmed", "status_unavailable", "bridge_restarted", "voice_disconnected", "voice_unavailable", "duration_limit", "media_unavailable", "invalid_voice_audio", "media_backpressure", "playback_backpressure", "invalid_media", "bridge_shutdown"])("preserves safe bridge error %s", async error => {
  const f = fixture();
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, error }));
  expect(await f.tool.handler(input, context())).toEqual({ ...snapshot, error });
});
it("bounds response bytes even when chunked and content-length lies", async () => {
  const f = fixture();
  const cancel = vi.fn();
  f.fetcher.mockResolvedValue(new Response(new ReadableStream({
    pull(controller) { controller.enqueue(new Uint8Array(400_000)); }, cancel,
  }), { headers: { "content-length": "1" } }));
  await expect(f.tool.handler(input, context())).rejects.toThrow("outcome may be unknown");
  expect(cancel).toHaveBeenCalledOnce();
  expect(f.fetcher).toHaveBeenCalledOnce();
});
it("accepts transcript limits and strips unknown fields/errors", async () => {
  const f = fixture();
  const transcript = Array.from({ length: 200 }, () => ({ speaker: "user", text: "a".repeat(4000) }));
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, transcript, error: token, internal: token }));
  expect(await f.tool.handler(input, context())).toEqual({ ...snapshot, transcript });
});

it("preserves the managed cloud gateway prefix", async () => {
  const f = fixture();
  f.config.NANOCODEX_PHONE_BRIDGE_URL = "https://nanocodex.gakonst.workers.dev/v1/phone/bridge";
  await f.tool.handler(input, context());
  expect(f.fetcher.mock.calls[0]![0]).toBe("https://nanocodex.gakonst.workers.dev/v1/phone/bridge/calls");
});

it("exposes the retained call thread identity without projecting arbitrary fields", async () => {
  const f = fixture();
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, call_agent_id: id, private: "hidden" }));
  expect(await f.tool.handler(input, context())).toEqual({ ...snapshot, call_agent_id: id });
  f.fetcher.mockResolvedValue(Response.json({ ...snapshot, call_agent_id: "../other" }));
  expect(await f.tool.handler(input, context())).toEqual(snapshot);
});

it.each([undefined, "", "other"])("requires the deployment phone admin at discovery and invocation: %s", async admin => {
  const f = fixture();
  f.config.NANOCODEX_PHONE_ADMIN_ID = admin;
  expect(phoneTools({ config: f.config, owner: "owner", agentId: id, authorize() {} })).toEqual([]);
  for (const value of [input, { operation: "status", call_id: id }, { operation: "hangup", call_id: id }])
    await expect(f.tool.handler(value, context())).rejects.toThrow("unavailable");
  expect(f.fetcher).not.toHaveBeenCalled();
});

it("bridge admission requires an explicitly selected phone admin matching the configured owner", () => {
  expect(phoneAdminConfigured({})).toBe(false);
  expect(phoneAdminConfigured({ NANOCODEX_PHONE_OWNER_ID: "owner" })).toBe(false);
  expect(phoneAdminConfigured({ NANOCODEX_PHONE_ADMIN_ID: "admin", NANOCODEX_PHONE_OWNER_ID: "owner" })).toBe(false);
  expect(phoneAdminConfigured({ NANOCODEX_PHONE_ADMIN_ID: "admin", NANOCODEX_PHONE_OWNER_ID: "admin" })).toBe(true);
});
