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
      return { iceServers: servers, relay: true };
    };
    const first = issue("first-credential");
    const response = await remoteICE(env, "remote-test-owner");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual(first);
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
});
