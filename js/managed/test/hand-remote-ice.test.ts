import { afterEach, describe, expect, it, vi } from "vitest";
import { remoteICE } from "../src/hand-remote-ice";

afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("Cloudflare TURN credential boundary", () => {
  it("uses the documented ICE endpoint, keeps the API token private, and refreshes its cache", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const env = { NANOCODEX_TURN_KEY_ID: "remote-test-key", NANOCODEX_TURN_API_TOKEN: "private-test-token" };
    const upstream = vi.fn(); vi.stubGlobal("fetch", upstream);
    const issue = (credential: string) => {
      const servers = [{ urls: ["stun:stun.cloudflare.com:3478"] },
        { urls: ["turn:turn.cloudflare.com:3478?transport=udp"], username: "short-lived-user", credential }];
      upstream.mockImplementationOnce(async (url: string, init: RequestInit) => {
        expect(url).toBe("https://rtc.live.cloudflare.com/v1/turn/keys/remote-test-key/credentials/generate-ice-servers");
        expect(init).toMatchObject({ method: "POST", redirect: "manual",
          headers: { authorization: "Bearer private-test-token", "content-type": "application/json" },
          body: JSON.stringify({ ttl: 3600, customIdentifier: "remote-test-owner" }),
        });
        return Response.json({ iceServers: servers }, { status: 201 });
      });
      return { iceServers: servers, relay: true, expires_at: Date.now() + 3600_000 };
    };
    const first = issue("first-credential");
    const response = await remoteICE(env, "remote-test-owner");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual(first);
    vi.setSystemTime(Date.now() + 60_000);
    expect(await (await remoteICE(env, "remote-test-owner")).json()).toEqual(first);
    expect(upstream).toHaveBeenCalledTimes(1);
    vi.setSystemTime(Date.now() + 10 * 60_000 + 1);
    const refreshed = issue("refreshed-credential");
    expect(await (await remoteICE(env, "remote-test-owner")).json()).toEqual(refreshed);
    expect(upstream).toHaveBeenCalledTimes(2);
  });

  it("returns STUN without configured relay secrets and reports a failed relay request", async () => {
    expect(await (await remoteICE({}, "stun-owner")).json()).toEqual({
      iceServers: [{ urls: ["stun:stun.cloudflare.com:3478"] }], relay: false,
    });
    vi.stubGlobal("fetch", vi.fn(async () => new Response("provider failure", { status: 503 })));
    const response = await remoteICE({ NANOCODEX_TURN_KEY_ID: "failing-key", NANOCODEX_TURN_API_TOKEN: "private-test-token" }, "failed-owner");
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: "remote_relay_unavailable" });
  });

  it("coalesces simultaneous host and viewer setup while keeping responses independent", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const issuedAt = Date.now();
    const env = { NANOCODEX_TURN_KEY_ID: "concurrent-key", NANOCODEX_TURN_API_TOKEN: "test-token" };
    let finish!: (response: Response) => void;
    const upstream = vi.fn(() => new Promise<Response>(resolve => { finish = resolve; }));
    vi.stubGlobal("fetch", upstream);
    const host = remoteICE(env, "same-owner");
    const viewer = remoteICE(env, "same-owner");
    expect(upstream).toHaveBeenCalledTimes(1);
    const iceServers = [{ urls: ["turn:turn.example:3478"], username: "temporary", credential: "credential" }];
    vi.setSystemTime(issuedAt + 4500);
    finish(Response.json({ iceServers }));
    const responses = await Promise.all([host, viewer]);
    expect(responses[0]).not.toBe(responses[1]);
    expect(await Promise.all(responses.map(response => response.json())))
      .toEqual([
        { iceServers, relay: true, expires_at: issuedAt + 3600_000 },
        { iceServers, relay: true, expires_at: issuedAt + 3600_000 },
      ]);
    expect(responses.every(response => response.headers.get("cache-control") === "no-store")).toBe(true);
  });

  it("isolates in-flight credentials by owner and TURN key", async () => {
    const completions: ((response: Response) => void)[] = [];
    const upstream = vi.fn(() => new Promise<Response>(resolve => { completions.push(resolve); }));
    vi.stubGlobal("fetch", upstream);
    const env = { NANOCODEX_TURN_KEY_ID: "isolation-key", NANOCODEX_TURN_API_TOKEN: "test-token" };
    const requests = [remoteICE(env, "owner-a"), remoteICE(env, "owner-b"),
      remoteICE({ ...env, NANOCODEX_TURN_KEY_ID: "other-key" }, "owner-a")];
    expect(upstream).toHaveBeenCalledTimes(3);
    completions.forEach((finish, index) => finish(Response.json({ iceServers: [
      { urls: ["turn:turn.example:3478"], username: "temporary", credential: `credential-${index}` },
    ] })));
    const bodies = await Promise.all(requests.map(async request => (await request).json() as Promise<any>));
    expect(bodies.map(body => body.iceServers[0].credential)).toEqual(["credential-0", "credential-1", "credential-2"]);
  });

  it("shares failures but permits an immediate successful retry", async () => {
    const env = { NANOCODEX_TURN_KEY_ID: "retry-key", NANOCODEX_TURN_API_TOKEN: "test-token" };
    let finish!: (response: Response) => void;
    const upstream = vi.fn(() => new Promise<Response>(resolve => { finish = resolve; }));
    vi.stubGlobal("fetch", upstream);
    const attempts = [remoteICE(env, "retry-owner"), remoteICE(env, "retry-owner")];
    finish(new Response("unavailable", { status: 503 }));
    expect((await Promise.all(attempts)).map(response => response.status)).toEqual([503, 503]);
    expect(upstream).toHaveBeenCalledTimes(1);
    const retry = remoteICE(env, "retry-owner");
    expect(upstream).toHaveBeenCalledTimes(2);
    finish(Response.json({ iceServers: [{ urls: ["turn:turn.example:3478"], username: "new", credential: "new" }] }));
    expect((await retry).status).toBe(200);
  });


  it("bounds concurrent generations without evicting an in-flight owner's shared work", async () => {
    const env = { NANOCODEX_TURN_KEY_ID: "capacity-key", NANOCODEX_TURN_API_TOKEN: "test-token" };
    const completions: ((response: Response) => void)[] = [];
    const upstream = vi.fn(() => new Promise<Response>(resolve => { completions.push(resolve); }));
    vi.stubGlobal("fetch", upstream);
    const requests = Array.from({ length: 256 }, (_, i) => remoteICE(env, `capacity-owner-${i}`));
    const shared = remoteICE(env, "capacity-owner-0");
    expect((await remoteICE(env, "capacity-overflow")).status).toBe(503);
    expect(upstream).toHaveBeenCalledTimes(256);
    completions.forEach(finish => finish(new Response(null, { status: 503 })));
    expect((await Promise.all([...requests, shared])).every(response => response.status === 503)).toBe(true);
    const retry = remoteICE(env, "capacity-overflow");
    expect(upstream).toHaveBeenCalledTimes(257);
    completions[256]!(Response.json({ iceServers: [{ urls: ["turn:turn.example:3478"] }] }));
    expect((await retry).status).toBe(200);
  });

});
