import { createExecutionContext } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, handleManagedRealtimeCall, ManagedRealtimeEgress, type EgressEnv } from "../src/egress";

const regions = ["wnam", "enam", "weur", "eeur", "apac", "oc", "sam"];
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function fixture(region?: string, regional = false) {
  const relay = vi.fn(async (_request: Request) => new Response("v=0\r\n", { status: 201 }));
  const get = vi.fn(() => ({ fetch: relay }));
  const idFromName = vi.fn((name: string) => name);
  const credential = { kind: "chatgpt", revision: 1, secret: "private-test-token", accountId: "test-account" };
  const env = {
    AGENT_SUBJECTS: { getByName: () => ({ fetch: async () => Response.json({ user_id: "test-user" }) }) },
    USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: async () => ({ status: 200, credential }) }) },
    CHATGPT_EGRESS: { idFromName, get },
  } as unknown as EgressEnv;
  const regionalNamespaces = Object.fromEntries(regions.map(region => {
    const fetch = vi.fn(async (_request: Request) => new Response("v=0\r\n", { status: 201 }));
    const idFromName = vi.fn((name: string) => name);
    const get = vi.fn(() => ({ fetch }));
    if (regional) Object.assign(env, { [`CHATGPT_EGRESS_${region.toUpperCase()}`]: { idFromName, get } });
    return [region, { fetch, idFromName, get }];
  }));
  const request = new Request("https://nanocodex.internal/v1/realtime/calls", {
    method: "POST",
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "x-nanocodex-subject": "a".repeat(64), "content-type": "application/json",
      "openai-alpha": "quicksilver=v2", "session-id": "test-session",
      "thread-id": "test-session", "x-session-id": "test-session",
      ...(region === undefined ? {} : { "x-nanocodex-voice-region": region }),
    },
    body: '{"sdp":"v=0"}',
  });
  return { env, request, relay, get, idFromName, regionalNamespaces };
}

describe("regional subscription voice relay", () => {
  it.each(regions)("selects the constrained %s pool with an isolated voice identity", async (region) => {
    const f = fixture(region, true);
    const selected = f.regionalNamespaces[region]!;
    const response = await handleEgress(f.request, f.env);
    expect(response.status).toBe(201);
    expect(await response.text()).toBe("v=0\r\n");
    expect(selected.get).toHaveBeenCalledWith(`voice-v1:${region}:test-user`, { locationHint: region });
    expect(f.get).not.toHaveBeenCalled();
    for (const [other, relay] of Object.entries(f.regionalNamespaces)) {
      if (other !== region) expect(relay.get).not.toHaveBeenCalled();
    }
    const sent = selected.fetch.mock.calls[0]![0];
    expect(sent.headers.has("x-nanocodex-voice-region")).toBe(false);
    expect(sent.headers.get("authorization")).toBe("Bearer private-test-token");
    expect(await sent.text()).toBe('{"sdp":"v=0"}');
  });

  it.each([false, true])("preserves regional RPC SDP/status/headers and never retries uncertain failures (%s)", async (fail) => {
    const f = fixture("wnam", true);
    f.env.CHATGPT_VOICE_RELAY_RPC = "true";
    const selected = f.regionalNamespaces.wnam!;
    const createRealtimeCall = vi.fn(async (_body: string, _headers: Record<string, string>, _search: string) => {
      if (fail) throw new Error("RPC interrupted after provider accepted call");
      return { status: 201, headers: { "content-type": "application/sdp", location: "/calls/rtc_fixture" }, body: "answer SDP" };
    });
    selected.get.mockReturnValue({ fetch: selected.fetch, createRealtimeCall } as ReturnType<typeof selected.get>);
    const response = await handleEgress(f.request, f.env);
    expect(response.status).toBe(fail ? 502 : 201);
    if (!fail) {
      expect(response.headers.get("location")).toBe("/calls/rtc_fixture");
      expect(response.headers.get("content-type")).toBe("application/sdp");
      expect(await response.text()).toBe("answer SDP");
    }
    expect(createRealtimeCall).toHaveBeenCalledTimes(1);
    expect(createRealtimeCall.mock.calls[0]![0]).toBe('{"sdp":"v=0"}');
    expect(createRealtimeCall.mock.calls[0]![1]["x-nanocodex-voice-region"]).toBeUndefined();
    expect(createRealtimeCall.mock.calls[0]![2]).toBe("?intent=quicksilver&architecture=avas");
    expect(selected.fetch).not.toHaveBeenCalled();
    expect(f.get).not.toHaveBeenCalled();
  });

  it.each([401, 429])("retains regional call identity and body through %s credential recovery", async (status) => {
    const f = fixture("weur", true);
    const selected = f.regionalNamespaces.weur!;
    selected.fetch.mockResolvedValueOnce(Response.json({ error: { type: "usage_limit_reached", resets_in_seconds: 3600 } }, { status }));
    const credential = (secret: string, accountId: string, revision: number) => ({ status: 200, credential: { kind: "chatgpt", secret, accountId, revision } });
    const resolveModelCredential = vi.fn().mockResolvedValueOnce(credential("old-secret", "account-a", 1))
      .mockResolvedValueOnce(credential("new-secret", status === 429 ? "account-b" : "account-a", 2));
    const reportLimit = vi.fn(async () => Response.json({ available: true }));
    f.env.USER_CREDENTIALS = { getByName: () => ({ resolveModelCredential, fetch: reportLimit }) } as unknown as EgressEnv["USER_CREDENTIALS"];
    expect((await handleEgress(f.request, f.env)).status).toBe(201);
    expect(selected.idFromName.mock.calls).toEqual(Array(2).fill(["voice-v1:weur:test-user"]));
    expect(selected.fetch.mock.calls.map(([sent]) => sent.headers.get("authorization"))).toEqual(["Bearer old-secret", "Bearer new-secret"]);
    expect(await selected.fetch.mock.calls[0]![0].text()).toBe(await selected.fetch.mock.calls[1]![0].text());
    expect(resolveModelCredential.mock.calls[1]).toEqual(status === 401 ? [true, 1, undefined] : [false, undefined, undefined]);
    expect(reportLimit).toHaveBeenCalledTimes(status === 429 ? 1 : 0);
    expect(f.get).not.toHaveBeenCalled();
  });

  it("keeps sideband on its direct provider route and call ID through authentication recovery", async () => {
    const f = fixture("wnam", true);
    const headers = new Headers(f.request.headers);
    headers.set("upgrade", "websocket");
    headers.set("x-nanocodex-realtime-call-id", "rtc_fixture");
    const request = new Request("https://nanocodex.internal/v1/realtime/sideband", { headers });
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    const upgrade = new Response(null, { status: 101, webSocket: client });
    const upstream = vi.fn(async (_request: Request) => upgrade).mockResolvedValueOnce(new Response(null, { status: 401 }));
    const response = await handleEgress(request, f.env, undefined, upstream as typeof fetch);
    expect(response).toBe(upgrade);
    expect(response.webSocket).toBe(client);
    expect(upstream).toHaveBeenCalledTimes(2);
    for (const [sent] of upstream.mock.calls) {
      expect(sent.url).toBe("https://api.openai.com/v1/live/rtc_fixture");
      expect(sent.headers.get("x-session-id")).toBe("test-session");
      expect(sent.headers.has("x-nanocodex-voice-region")).toBe(false);
      expect(sent.headers.has("x-nanocodex-realtime-call-id")).toBe(false);
    }
    for (const relay of Object.values(f.regionalNamespaces)) expect(relay.get).not.toHaveBeenCalled();
    expect(f.get).not.toHaveBeenCalled();
    client.accept(); client.close(); server.close();
  });

  it.each(["configured", "direct", "openai"])("preserves %s voice routing policy with regional pools configured", async (mode) => {
    const f = fixture("wnam", mode !== "direct");
    if (mode === "configured") f.env.CODEX_RELAY_URL = "https://relay.example.test";
    if (mode === "direct") delete f.env.CHATGPT_EGRESS;
    if (mode === "openai") f.env.USER_CREDENTIALS = { getByName: () => ({ resolveModelCredential: async () => ({ status: 200,
      credential: { kind: "openai", secret: "fixture-secret", revision: 1 } }) }) } as unknown as EgressEnv["USER_CREDENTIALS"];
    const upstream = vi.fn(async (_request: Request) => new Response("answer", { status: 201 }));
    const response = await handleEgress(f.request, f.env, undefined, upstream as typeof fetch);
    expect(response.status).toBe(mode === "openai" ? 409 : 201);
    expect(upstream).toHaveBeenCalledTimes(mode === "openai" ? 0 : 1);
    for (const [sent] of upstream.mock.calls) expect(sent.headers.has("x-nanocodex-voice-region")).toBe(false);
    for (const relay of Object.values(f.regionalNamespaces)) expect(relay.get).not.toHaveBeenCalled();
    expect(f.get).not.toHaveBeenCalled();
  });

  it.each(["0", "1"])("keeps sampled transport stable for voice session ending %s", async (last) => {
    const f = fixture("wnam");
    f.env.CHATGPT_VOICE_RELAY_RPC = "sample";
    f.request.headers.set("x-session-id", `11111111-1111-4111-8111-11111111111${last}`);
    const createRealtimeCall = vi.fn(async () => ({ status: 201, headers: {}, body: "answer SDP" }));
    f.get.mockReturnValue({ fetch: f.relay, createRealtimeCall } as ReturnType<typeof f.get>);
    expect((await handleEgress(f.request, f.env)).status).toBe(201);
    expect(createRealtimeCall).toHaveBeenCalledTimes(last === "0" ? 1 : 0);
    expect(f.relay).toHaveBeenCalledTimes(last === "0" ? 0 : 1);
  });
  it("transfers the complete SDP exchange through the private relay RPC when enabled", async () => {
    const f = fixture("wnam");
    f.env.CHATGPT_VOICE_RELAY_RPC = "true";
    const createRealtimeCall = vi.fn(async (_body: string, _headers: Record<string, string>, _search: string) => ({
      status: 201, headers: { "content-type": "application/sdp", location: "/calls/fixture" }, body: "answer SDP",
    }));
    f.get.mockReturnValue({ fetch: f.relay, createRealtimeCall } as ReturnType<typeof f.get>);
    const response = await handleEgress(f.request, f.env);
    expect(response.status).toBe(201);
    expect(response.headers.get("location")).toBe("/calls/fixture");
    expect(await response.text()).toBe("answer SDP");
    expect(createRealtimeCall).toHaveBeenCalledTimes(1);
    expect(createRealtimeCall.mock.calls[0]![0]).toBe('{"sdp":"v=0"}');
    expect(createRealtimeCall.mock.calls[0]![1].authorization).toBe("Bearer private-test-token");
    expect(createRealtimeCall.mock.calls[0]![1]["x-nanocodex-voice-region"]).toBeUndefined();
    expect(createRealtimeCall.mock.calls[0]![2]).toBe("?intent=quicksilver&architecture=avas");
    expect(f.relay).not.toHaveBeenCalled();
  });
  it("does not create a second provider call after a relay RPC failure", async () => {
    const f = fixture("wnam");
    f.env.CHATGPT_VOICE_RELAY_RPC = "true";
    const createRealtimeCall = vi.fn(async () => { throw new Error("RPC interrupted after provider accepted call"); });
    f.get.mockReturnValue({ fetch: f.relay, createRealtimeCall } as ReturnType<typeof f.get>);
    expect((await handleEgress(f.request, f.env)).status).toBe(502);
    expect(createRealtimeCall).toHaveBeenCalledTimes(1);
    expect(f.relay).not.toHaveBeenCalled();
  });
  it.each(["wnam", "enam", "weur", "eeur", "apac", "oc", "sam"])(
    "uses an isolated per-user %s relay without forwarding the placement header", async (region) => {
      const f = fixture(region);
      expect((await handleEgress(f.request, f.env)).status).toBe(201);
      expect(f.idFromName).toHaveBeenCalledWith(`voice-v1:${region}:test-user`);
      expect(f.get).toHaveBeenCalledWith(`voice-v1:${region}:test-user`, { locationHint: region });
      expect(f.relay.mock.calls[0]![0].headers.has("x-nanocodex-voice-region")).toBe(false);
      expect(f.relay.mock.calls[0]![0].headers.get("authorization")).toBe("Bearer private-test-token");
    },
  );
  it.each([undefined, "invalid", "WNAM", "wnam,enam", "afr", ""])("retains the existing relay for an absent/invalid hint (%s)", async (region) => {
    const f = fixture(region, true);
    expect((await handleEgress(f.request, f.env)).status).toBe(201);
    expect(f.idFromName).toHaveBeenCalledWith("user-v1:test-user");
    for (const relay of Object.values(f.regionalNamespaces)) expect(relay.get).not.toHaveBeenCalled();
  });
  it("still denies unavailable ownership before starting a regional relay", async () => {
    const f = fixture("wnam");
    f.env.AGENT_SUBJECTS = { getByName: () => ({ fetch: async () => new Response(null, { status: 404 }) }) } as unknown as EgressEnv["AGENT_SUBJECTS"];
    expect((await handleEgress(f.request, f.env)).status).toBe(403);
    expect(f.get).not.toHaveBeenCalled();
  });
});

describe("private managed voice ownership capability", () => {
  const subject = `managed-session-v1_${"a".repeat(64)}`;
  const owner = "11111111-1111-4111-8111-111111111111";
  it("returns complete SDP through private RPC and retains credential/ownership policy", async () => {
    const f = fixture("wnam");
    f.request.headers.set("x-nanocodex-subject", subject);
    f.request.headers.set("x-nanocodex-realtime-owner", owner);
    const entrypoint = new ManagedRealtimeEgress(createExecutionContext(), f.env);
    const reply = await entrypoint.createCall(await f.request.text(), Object.fromEntries(f.request.headers));
    expect(reply.status).toBe(201);
    expect(reply.body).toBe("v=0\r\n");
    expect(JSON.stringify(reply)).not.toContain("private-test-token");
    const denied = await entrypoint.createCall("{}", {});
    expect(denied.status).toBe(403);
    expect(f.relay).toHaveBeenCalledTimes(1);
  });
  it.each([subject, "a".repeat(64)])("uses the ingress's verified owner without a directory lookup for %s", async (subject) => {
    const f = fixture("wnam");
    f.request.headers.set("x-nanocodex-subject", subject);
    f.request.headers.set("x-nanocodex-realtime-owner", owner);
    const directory = vi.fn(async () => { throw new Error("directory must not be read"); });
    f.env.AGENT_SUBJECTS = { getByName: directory } as unknown as EgressEnv["AGENT_SUBJECTS"];
    const credentials = vi.fn(async () => ({ status: 200, credential: {
      kind: "chatgpt", revision: 1, secret: "private-test-token", accountId: "test-account" },
    }));
    const getByName = vi.fn(() => ({ resolveModelCredential: credentials }));
    f.env.USER_CREDENTIALS = { getByName } as unknown as EgressEnv["USER_CREDENTIALS"];
    expect((await handleManagedRealtimeCall(f.request, f.env)).status).toBe(201);
    expect(getByName).toHaveBeenCalledWith(owner, { locationHint: "wnam" });
    expect(f.env).not.toHaveProperty("trustedPlacementRegion");
    expect(directory).not.toHaveBeenCalled();
    expect(f.relay.mock.calls[0]![0].headers.has("x-nanocodex-realtime-owner")).toBe(false);
  });
  it.each(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc", "WNAM", "SJC", "wnam,weur", "", "invalid"])("uses only validated private voice hints for broker first touch (%s)", async region => {
    const f = fixture(region);
    f.request.headers.set("x-nanocodex-subject", subject);
    f.request.headers.set("x-nanocodex-realtime-owner", owner);
    f.request.headers.set("x-nanocodex-placement-colo", "NRT");
    const getByName = vi.fn(() => ({ resolveModelCredential: async () => ({ status: 401, error: "credential_not_found" }) }));
    f.env.USER_CREDENTIALS = { getByName } as unknown as EgressEnv["USER_CREDENTIALS"];
    const reply = await handleManagedRealtimeCall(f.request, f.env);
    expect(reply.status).toBe(503);
    await reply.body?.cancel();
    expect(getByName).toHaveBeenCalledWith(owner, ["wnam", "enam", "sam", "weur", "eeur", "apac", "oc"].includes(region) ? { locationHint: region } : undefined);
    expect(f.env).not.toHaveProperty("trustedPlacementRegion");
  });
  it("ignores public voice and placement assertions for credential broker placement", async () => {
    const f = fixture("wnam");
    f.request.headers.set("x-nanocodex-placement-colo", "NRT");
    const getByName = vi.fn(() => ({ resolveModelCredential: async () => ({ status: 401, error: "credential_not_found" }) }));
    f.env.USER_CREDENTIALS = { getByName } as unknown as EgressEnv["USER_CREDENTIALS"];
    const reply = await handleEgress(f.request, f.env);
    expect(reply.status).toBe(503);
    await reply.body?.cancel();
    expect(getByName).toHaveBeenCalledWith("test-user", undefined);
  });
  it.each([subject, "a".repeat(64)])("does not trust the owner header on generic agent egress for %s", async (subject) => {
    const f = fixture("wnam");
    f.request.headers.set("x-nanocodex-subject", subject);
    f.request.headers.set("x-nanocodex-realtime-owner", owner);
    f.env.MANAGED_AGENT_OWNERSHIP = {
      fetch: async () => new Response(null, { status: 404 }),
    } as unknown as Fetcher;
    f.env.AGENT_SUBJECTS = { getByName: () => ({ fetch: async () => new Response(null, { status: 404 }) }) } as unknown as EgressEnv["AGENT_SUBJECTS"];
    expect((await handleEgress(f.request, f.env)).status).toBe(403);
    expect(f.relay).not.toHaveBeenCalled();
  });
  it.each(["owner", "subject", "path", "placeholder"])("rejects invalid %s before touching a relay", async (invalid) => {
    const f = fixture("wnam");
    f.request.headers.set("x-nanocodex-subject", subject);
    f.request.headers.set("x-nanocodex-realtime-owner", owner);
    if (invalid === "owner") f.request.headers.set("x-nanocodex-realtime-owner", "arbitrary-user");
    if (invalid === "subject") f.request.headers.set("x-nanocodex-subject", "a".repeat(63));
    if (invalid === "placeholder") f.request.headers.set("authorization", "Bearer untrusted");
    const request = invalid === "path" ? new Request("https://nanocodex.internal/v1/responses", f.request) : f.request;
    expect((await handleManagedRealtimeCall(request, f.env)).status).toBe(403);
    expect(f.relay).not.toHaveBeenCalled();
  });
});
