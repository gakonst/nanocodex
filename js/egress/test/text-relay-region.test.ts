import { createExecutionContext } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { handleEgress, SessionModelEgress, type EgressEnv } from "../src/egress";

const owner = "11111111-1111-4111-8111-111111111111";
const subject = `managed-session-v1_${"a".repeat(64)}`;
const ownerHeader = "x-nanocodex-session-model-owner";
const regionHeader = "x-nanocodex-model-region";
const regions = ["wnam", "enam", "sam", "weur", "eeur", "apac", "oc"];
function request(region?: string, method = "POST", user = owner) {
  return new Request("https://nanocodex.internal/v1/responses", { method, headers: {
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", [ownerHeader]: user,
    "x-nanocodex-subject": subject, ...(region === undefined ? {} : { [regionHeader]: region }),
    ...(method === "GET" ? { upgrade: "websocket", "openai-beta": "responses_websockets=2026-02-06" }
      : { "content-type": "application/json" }),
  }, ...(method === "POST" ? { body: '{"input":[],"stream":true}' } : {}) });
}
function fixture(regional = false) {
  const relay = vi.fn(async (_request: Request) => new Response(null, { status: 204 }));
  const idFromName = vi.fn((name: string) => name);
  const get = vi.fn(() => ({ fetch: relay }));
  const lookup = vi.fn(async (recover: boolean) => ({ status: 200, credential: {
    kind: "chatgpt", revision: recover ? 2 : 1, secret: recover ? "new-fixture-secret" : "fixture-secret", accountId: "fixture-account",
  } }));
  const getByName = vi.fn(() => ({ resolveModelCredential: lookup }));
  const callback = vi.fn(async () => { throw new Error("unexpected ownership roundtrip"); });
  const env = { USER_CREDENTIALS: { getByName }, MANAGED_AGENT_OWNERSHIP: { fetch: callback },
    CHATGPT_EGRESS: { idFromName, get } } as unknown as EgressEnv;
  const regionalNamespaces = Object.fromEntries(regions.map(region => {
    const fetch = vi.fn(async (_request: Request) => new Response(null, { status: 204 }));
    const idFromName = vi.fn((name: string) => name);
    const get = vi.fn(() => ({ fetch }));
    if (regional) Object.assign(env, { [`CHATGPT_EGRESS_${region.toUpperCase()}`]: { idFromName, get } });
    return [region, { fetch, idFromName, get }];
  }));
  const entrypoint = new SessionModelEgress(createExecutionContext(), env);
  return { env, entrypoint, relay, idFromName, get, lookup, getByName, callback, regionalNamespaces };
}
function captureLog() { return vi.spyOn(console, "info").mockImplementation(() => {}); }
let log: ReturnType<typeof captureLog>;
beforeEach(() => { log = captureLog(); for (const method of ["warn", "error"] as const) vi.spyOn(console, method).mockImplementation(() => {}); });
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("private regional ChatGPT text relay", () => {
  it.each([["wnam", "POST"], ["weur", "GET"]])(
    "selects the constrained %s application for trusted %s text", async (region, method) => {
      const f = fixture(true);
      expect((await f.entrypoint.fetch(request(region, method))).status).toBe(204);
      expect(f.regionalNamespaces[region!]!.get).toHaveBeenCalledWith(`text-v1:${region}:${owner}`, { locationHint: region });
      expect(f.get).not.toHaveBeenCalled();
      for (const [other, relay] of Object.entries(f.regionalNamespaces)) {
        if (other !== region) expect(relay.get).not.toHaveBeenCalled();
      }
      const sent = f.regionalNamespaces[region!]!.fetch.mock.calls[0]![0];
      expect(sent.headers.get("authorization")).toBe("Bearer fixture-secret");
      for (const header of [ownerHeader, regionHeader, "x-nanocodex-subject"]) expect(sent.headers.has(header)).toBe(false);
    },
  );
  it("preserves the regional relay WebSocket protocol through the existing failover wrapper", async () => {
    const f = fixture(true);
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    const upgrade = new Response(null, { status: 101, webSocket: client });
    f.regionalNamespaces.wnam!.fetch.mockResolvedValueOnce(upgrade);
    const response = await f.entrypoint.fetch(request("wnam", "GET"));
    expect(response.status).toBe(101);
    const socket = response.webSocket!;
    socket.accept();
    const receive = (peer: WebSocket) => new Promise<string>((resolve) => peer.addEventListener("message", (event) => resolve(String(event.data)), { once: true }));
    const upstreamFrame = receive(server);
    const create = '{"type":"response.create","response":{"input":[]}}';
    socket.send(create);
    expect(await upstreamFrame).toBe(create);
    const downstreamFrame = receive(socket);
    const complete = '{"type":"response.completed","response":{"id":"resp_fixture"}}';
    server.send(complete);
    expect(await downstreamFrame).toBe(complete);
    socket.close(); server.close();
  });

  it.each([["wnam", "POST"], ["weur", "GET"]])(
    "selects a new isolated %s relay for %s and strips private placement", async (region, method) => {
      const f = fixture();
      expect((await f.entrypoint.fetch(request(region, method))).status).toBe(204);
      expect(f.get).toHaveBeenCalledWith(`text-v1:${region}:${owner}`, { locationHint: region });
      expect(f.getByName).toHaveBeenCalledWith(owner, { locationHint: region });
      expect(f.callback).not.toHaveBeenCalled();
      const sent = f.relay.mock.calls[0]![0];
      expect(sent.headers.get("authorization")).toBe("Bearer fixture-secret");
      for (const header of [ownerHeader, regionHeader, "x-nanocodex-subject"]) expect(sent.headers.has(header)).toBe(false);
      expect(log).toHaveBeenCalledWith(expect.objectContaining({ rule: "responses", relay_region: region }));
    },
  );
  it.each([undefined, "wnam,weur"])("falls back to the unchanged legacy identity for %j", async (region) => {
    const f = fixture(true);
    expect((await f.entrypoint.fetch(request(region))).status).toBe(204);
    for (const relay of Object.values(f.regionalNamespaces)) expect(relay.get).not.toHaveBeenCalled();
    expect(f.get).toHaveBeenCalledWith(`user-v1:${owner}`, undefined);
    expect(f.relay.mock.calls[0]![0].headers.has(regionHeader)).toBe(false);
    expect(log).not.toHaveBeenCalledWith(expect.objectContaining({ relay_region: expect.anything() }));
  });
  it("keeps the region/user identity stable across calls and one 401 recovery with fresh credentials", async () => {
    const f = fixture(true);
    const selected = f.regionalNamespaces.wnam!;
    selected.fetch.mockResolvedValueOnce(new Response(null, { status: 401 }));
    expect((await f.entrypoint.fetch(request("wnam"))).status).toBe(204);
    expect((await f.entrypoint.fetch(request("wnam"))).status).toBe(204);
    expect(selected.idFromName.mock.calls).toEqual(Array(3).fill([`text-v1:wnam:${owner}`]));
    expect(f.lookup.mock.calls).toEqual([[false, undefined, undefined], [true, 1, undefined], [false, undefined, undefined]]);
    expect(selected.fetch.mock.calls.map(([sent]) => sent.headers.get("authorization"))).toEqual([
      "Bearer fixture-secret", "Bearer new-fixture-secret", "Bearer fixture-secret",
    ]);
    expect(await selected.fetch.mock.calls[0]![0].text()).toBe(await selected.fetch.mock.calls[1]![0].text());
    const other = "22222222-2222-4222-8222-222222222222";
    expect((await f.entrypoint.fetch(request("wnam", "POST", other))).status).toBe(204);
    expect(selected.idFromName).toHaveBeenLastCalledWith(`text-v1:wnam:${other}`);
    expect((await f.entrypoint.fetch(request("weur"))).status).toBe(204);
    expect(f.regionalNamespaces.weur!.idFromName).toHaveBeenLastCalledWith(`text-v1:weur:${owner}`);
  });
  it("ignores a regional header on generic egress even after valid ownership resolution", async () => {
    const f = fixture(true);
    f.env.MANAGED_AGENT_OWNERSHIP = { fetch: async () => Response.json({ user_id: owner }) } as unknown as Fetcher;
    const input = request("wnam"); input.headers.delete(ownerHeader);
    expect((await handleEgress(input, f.env)).status).toBe(204);
    expect(f.get).toHaveBeenCalledWith(`user-v1:${owner}`, undefined);
    expect(f.relay.mock.calls[0]![0].headers.has(regionHeader)).toBe(false);
  });
  it("does not start a relay after authority or credential denial", async () => {
    const f = fixture();
    const input = request("wnam"); input.headers.delete(ownerHeader);
    expect((await f.entrypoint.fetch(input)).status).toBe(403);
    expect(f.lookup).not.toHaveBeenCalled();
    f.env.USER_CREDENTIALS = { getByName: () => ({ resolveModelCredential: async () => ({ status: 404 }) }) } as unknown as EgressEnv["USER_CREDENTIALS"];
    expect((await f.entrypoint.fetch(request("wnam"))).status).toBeGreaterThanOrEqual(400);
    expect(f.get).not.toHaveBeenCalled();
  });
  it.each(["direct", "configured", "openai"])("does not select or leak regional placement for %s upstream", async (mode) => {
    const f = fixture(mode !== "direct");
    if (mode === "direct") delete f.env.CHATGPT_EGRESS;
    if (mode === "configured") f.env.CODEX_RELAY_URL = "https://relay.example.test";
    if (mode === "openai") f.env.USER_CREDENTIALS = { getByName: () => ({ resolveModelCredential: async () => ({ status: 200,
      credential: { kind: "openai", revision: 1, secret: "fixture-secret" } }) }) } as unknown as EgressEnv["USER_CREDENTIALS"];
    const upstream = vi.fn(async (sent: Request) => {
      expect(sent.headers.has(regionHeader)).toBe(false);
      expect(sent.headers.has(ownerHeader)).toBe(false);
      return new Response(null, { status: 204 });
    });
    vi.stubGlobal("fetch", upstream);
    expect((await f.entrypoint.fetch(request("wnam"))).status).toBe(204);
    expect(upstream).toHaveBeenCalledTimes(1);
    for (const relay of Object.values(f.regionalNamespaces)) expect(relay.get).not.toHaveBeenCalled();
    expect(f.get).not.toHaveBeenCalled();
    expect(log).not.toHaveBeenCalledWith(expect.objectContaining({ relay_region: expect.anything() }));
  });
});
