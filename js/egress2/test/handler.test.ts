import { describe, expect, it, vi } from "vitest";
import { createEgressHandler, CREDENTIAL_PLACEHOLDER } from "../src/handler";

const endpoint = "https://api.openai.com/v1/responses";
function request(url = endpoint, owner = "user-a", authorization = `Bearer ${CREDENTIAL_PLACEHOLDER}`) {
  return new Request(url, {
    method: "POST",
    headers: { "x-managed2-owner": owner, authorization, "content-type": "application/json" },
    body: JSON.stringify({ model: "test", input: "hello" }),
  });
}

describe("private credential egress", () => {
  it("replaces the placeholder only for the host-asserted owner, strips owner, and streams the upstream response", async () => {
    const readCredential = vi.fn(async (owner: string) => owner === "user-a" ? "sk-example-A" : "sk-example-B");
    const body = new ReadableStream({ start(controller) { controller.enqueue(new TextEncoder().encode("event: ok\\n\\n")); controller.close(); } });
    const upstream = new Response(body, { headers: { "content-type": "text/event-stream" } });
    const upstreamFetch = vi.fn(async (outbound: Request) => {
      expect(outbound.headers.get("authorization")).toBe("Bearer sk-example-A");
      expect(outbound.headers.has("x-managed2-owner")).toBe(false);
      expect(outbound.redirect).toBe("manual");
      expect(outbound.url).toBe(endpoint);
      expect(await outbound.text()).toContain("hello");
      return upstream;
    });
    const proxy = createEgressHandler({ readCredential, upstreamFetch });
    const inbound = request();
    const clone = vi.spyOn(inbound, "clone");
    const result = await proxy.fetch(inbound, {});
    expect(clone).not.toHaveBeenCalled(); // no unnecessary tee/buffering on API-key path
    expect(result).not.toBe(upstream); // response metadata added without buffering the stream
    expect(result.body).toBe(body);
    expect(result.headers.get("server-timing")).toContain('egress_route;desc="openai_api"');
    expect(await result.text()).toBe("event: ok\\n\\n");
    expect(upstreamFetch).toHaveBeenCalledTimes(1);
  });

  it("reports deterministic cache-hit vs lookup and upstream-header durations without sensitive data", async () => {
    let tick = 0;
    const events: Record<string, string | number | null>[] = [];
    const readCredential = vi.fn(async () => { tick += 7; return "sk-never-log"; });
    const upstreamFetch = vi.fn(async (out: Request) => {
      tick += 23;
      return new Response("streamed-body-secret", { status: 201, headers: { "server-timing": 'bad;desc="sk-never-log"' } });
    });
    const proxy = createEgressHandler({ readCredential, upstreamFetch, clock: () => tick, log: event => events.push(event) });
    const inbound = request();
    inbound.headers.set("x-managed2-agent", "private-agent");
    inbound.headers.set("x-nanocodex-egress-route", "spoofed");
    inbound.headers.set("server-timing", "secret-private");
    const miss = await proxy.fetch(inbound, {});
    expect(miss.status).toBe(201);
    const outbound = upstreamFetch.mock.calls[0]?.[0];
    expect(outbound?.headers.get("authorization")).toBe("Bearer sk-never-log");
    expect(outbound?.headers.has("x-managed2-owner")).toBe(false);
    expect(outbound?.headers.has("x-managed2-agent")).toBe(false);
    expect(outbound?.headers.has("x-nanocodex-egress-route")).toBe(false);
    expect(outbound?.headers.has("server-timing")).toBe(false);
    expect(miss.headers.get("server-timing")).toBe('egress_credential;dur=7.0, egress_dispatch;dur=7.0, egress_upstream_headers;dur=23.0, egress_total;dur=30.0, egress_route;desc="openai_api", egress_cache;desc="miss"');
    expect(await miss.text()).toBe("streamed-body-secret");
    const hit = await proxy.fetch(inbound, {});
    expect(hit.headers.get("server-timing")).toBe('egress_credential;dur=0.0, egress_dispatch;dur=0.0, egress_upstream_headers;dur=23.0, egress_total;dur=23.0, egress_route;desc="openai_api", egress_cache;desc="hit"');
    expect(readCredential).toHaveBeenCalledOnce();
    expect(events).toEqual([
      { event: "responses_egress", route_kind: "openai_api", upstream_status: 201, credential_cache: "miss", credential_ms: 7, upstream_dispatch_ms: 7, upstream_headers_ms: 23, total_ms: 30 },
      { event: "responses_egress", route_kind: "openai_api", upstream_status: 201, credential_cache: "hit", credential_ms: 0, upstream_dispatch_ms: 0, upstream_headers_ms: 23, total_ms: 23 },
    ]);
    expect(JSON.stringify(events)).not.toMatch(/sk-never-log|user-a|private-agent|streamed-body-secret|secret-private|spoofed/);
  });

  it("caches by owner with 60-second TTL, bounded to 256 entries, and supports write invalidation", async () => {
    let time = 1000;
    const readCredential = vi.fn(async (owner: string) => `sk-${owner}`);
    const auths: string[] = [];
    const proxy = createEgressHandler({
      readCredential, now: () => time,
      upstreamFetch: async outgoing => { auths.push(outgoing.headers.get("authorization")!); return new Response("ok"); },
    });
    await proxy.fetch(request(), {});
    await proxy.fetch(request(), {});
    expect(readCredential).toHaveBeenCalledTimes(1);
    await proxy.fetch(request(endpoint, "user-b"), {});
    expect(auths).toEqual(["Bearer sk-user-a", "Bearer sk-user-a", "Bearer sk-user-b"]);
    time += 60_000;
    await proxy.fetch(request(), {});
    expect(readCredential).toHaveBeenCalledTimes(3);
    proxy.invalidate("user-a");
    await proxy.fetch(request(), {});
    expect(readCredential).toHaveBeenCalledTimes(4);
    for (let index = 0; index < 257; index++) await proxy.fetch(request(endpoint, `user-${index}`), {});
    await proxy.fetch(request(endpoint, "user-b"), {});
    expect(readCredential).toHaveBeenCalledTimes(262); // oldest owner was evicted
  });

  it("rejects wrong hosts, schemes, paths, methods, absent owner and absent placeholder before key lookup", async () => {
    const readCredential = vi.fn(async () => "sk-secret");
    const upstreamFetch = vi.fn(async () => new Response("unexpected"));
    const proxy = createEgressHandler({ readCredential, upstreamFetch });
    for (const url of ["https://api.openai.com.evil.test/v1/responses", "http://api.openai.com/v1/responses", "https://api.openai.com/v1/other"]) {
      expect((await proxy.fetch(request(url), {})).status).toBe(403);
    }
    expect((await proxy.fetch(new Request(endpoint, { method: "GET", headers: { "x-managed2-owner": "user-a", authorization: `Bearer ${CREDENTIAL_PLACEHOLDER}` } }), {})).status).toBe(405);
    expect((await proxy.fetch(request(endpoint, ""), {})).status).toBe(400);
    expect((await proxy.fetch(request(endpoint, "user-a", "Bearer wrong"), {})).status).toBe(400);
    expect(readCredential).not.toHaveBeenCalled();
    expect(upstreamFetch).not.toHaveBeenCalled();
  });

  it("fails closed for an owner without a stored credential", async () => {
    const upstreamFetch = vi.fn(async () => new Response("unexpected"));
    const proxy = createEgressHandler({ readCredential: async () => null, upstreamFetch });
    const result = await proxy.fetch(request(), {});
    expect(result.status).toBe(403);
    expect(upstreamFetch).not.toHaveBeenCalled();
  });

  it("passes through a WebSocket 101 response for a GET upgrade", async () => {
    // Node's Response rejects 101; a mock represents workerd's WebSocket Response.
    const switched = { status: 101, webSocket: { accept() {} } } as unknown as Response;
    const upstreamFetch = vi.fn(async (outgoing: Request) => {
      expect(outgoing.method).toBe("GET");
      expect(outgoing.headers.get("upgrade")).toBe("websocket");
      expect(outgoing.headers.get("authorization")).toBe("Bearer sk-example");
      expect(outgoing.redirect).toBe("manual");
      return switched;
    });
    const proxy = createEgressHandler({ readCredential: async () => "sk-example", upstreamFetch });
    const inbound = new Request(endpoint, {
      method: "GET",
      headers: { "upgrade": "websocket", "x-managed2-owner": "user-a", authorization: `Bearer ${CREDENTIAL_PLACEHOLDER}` },
    });
    expect(await proxy.fetch(inbound, {})).toBe(switched);
    expect(upstreamFetch).toHaveBeenCalledOnce();
  });

  it("never returns a redirect Location, credential-store error, or upstream fetch error to the caller", async () => {
    const redirect = new Response(null, { status: 302, headers: { location: "https://evil.test/sk-secret" } });
    const proxy = createEgressHandler({ readCredential: async () => "sk-secret", upstreamFetch: async () => redirect });
    const result = await proxy.fetch(request(), {});
    expect(result.status).toBe(502);
    expect(result.headers.get("location")).toBeNull();
    expect(await result.text()).not.toContain("sk-secret");
    const storageFailure = createEgressHandler({ readCredential: async () => { throw new Error("sk-secret"); } });
    expect(await (await storageFailure.fetch(request(), {})).text()).not.toContain("sk-secret");
    const networkFailure = createEgressHandler({ readCredential: async () => "sk-secret", upstreamFetch: async () => { throw new Error("sk-secret"); } });
    expect(await (await networkFailure.fetch(request(), {})).text()).not.toContain("sk-secret");
  });
});
