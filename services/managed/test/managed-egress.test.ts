import { afterEach, describe, expect, it, vi } from "vitest";

import {
  handleManagedEgress,
} from "../src/managed-egress";

const SUBJECT = "s".repeat(43);

afterEach(() => vi.unstubAllGlobals());

describe("Computer egress gateway", () => {
  it("sends provider reads and unbounded writes through the private connector binding", async () => {
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        seen.push(new Request(input, init));
        return Response.json({ ok: true });
      },
    } as Fetcher;
    const publicFetch = vi.fn(async () => new Response("public"));
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);

    for (const url of [
      "https://api.github.com/repos/nanocodex/nanocodex/pulls?state=open",
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10",
      "https://www.googleapis.com/drive/v3/files?pageSize=10",
      "https://api.x.com/2/dm_events?max_results=10",
      "https://api.prod.whoop.com/developer/v2/recovery?limit=10",
    ]) {
      expect((await gateway.fetch(url)).status).toBe(200);
    }
    const writeBody = "x".repeat(2 * 1024 * 1024);
    expect((await gateway.fetch("https://www.googleapis.com/upload/drive/v3/files?uploadType=media", {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: writeBody,
    })).status).toBe(200);
    const xWriteBody = JSON.stringify({ text: "hello from Nanocodex" });
    expect((await gateway.fetch("https://api.x.com/2/tweets", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: xWriteBody,
    })).status).toBe(200);
    expect((await gateway.fetch(
      "https://api.x.com/2/users/2244994945/bookmarks/1890000000000000000",
      { method: "DELETE" },
    )).status).toBe(200);
    expect((await gateway.fetch(
      "https://api.prod.whoop.com/developer/v2/activity/workout",
      { method: "POST", headers: { "content-type": "application/json" }, body: "{}" },
    )).status).toBe(403);

    expect(seen.map((request) => request.url)).toEqual([
      "https://api.github.com/repos/nanocodex/nanocodex/pulls?state=open",
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10",
      "https://www.googleapis.com/drive/v3/files?pageSize=10",
      "https://api.x.com/2/dm_events?max_results=10",
      "https://api.prod.whoop.com/developer/v2/recovery?limit=10",
      "https://www.googleapis.com/upload/drive/v3/files?uploadType=media",
      "https://api.x.com/2/tweets",
      "https://api.x.com/2/users/2244994945/bookmarks/1890000000000000000",
    ]);
    expect(seen.every((request) => (
      request.headers.get("authorization") === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
        && request.headers.get("x-nanocodex-subject") === SUBJECT
    ))).toBe(true);
    expect(publicFetch).not.toHaveBeenCalled();
    expect(await seen[5]!.text()).toBe(writeBody);
    expect(await seen[6]!.text()).toBe(xWriteBody);
    expect(seen[6]!.headers.get("content-type")).toBe("application/json");
  });

  it("rejects credentials, lookalikes, userinfo, and private destinations", async () => {
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    const publicFetch = vi.fn();
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);
    const cases: Array<[RequestInfo, RequestInit | undefined, number]> = [
      ["https://example.com/", { headers: { authorization: "Bearer browser-token" } }, 403],
      ["https://example.com/", { headers: { cookie: "session=secret" } }, 403],
      ["https://example.com/", { headers: { cookie2: "session=secret" } }, 403],
      ["https://example.com/", { headers: { "x-authorization2": "browser-token" } }, 403],
      ["http://api.github.com/user", undefined, 403],
      ["https://api.github.com.evil.test/user", undefined, 403],
      ["https://gmail.googleapis.com/gmail/v1/users/me/%2e%2e%2fother/messages", undefined, 403],
      ["https://www.googleapis.com/drive/v3/%252e%252e%252fother", undefined, 403],
      ["https://api.prod.whoop.com/oauth/oauth2/token", undefined, 403],
      ["https://api.prod.whoop.com/developer/v2/user/access", undefined, 403],
      ["https://api.prod.whoop.com/developer/v2/partner/token", undefined, 403],
      ["https://name:password@example.com/", undefined, 403],
      ["http://127.0.0.1/", undefined, 403],
      ["http://169.254.169.254/latest/meta-data", undefined, 403],
      ["http://[::1]/", undefined, 403],
    ];
    for (const [url, init, status] of cases) {
      expect((await gateway.fetch(url, init)).status, String(url)).toBe(status);
    }
    expect(binding.fetch).not.toHaveBeenCalled();
    expect(publicFetch).not.toHaveBeenCalled();
  });

  it("manually follows only revalidated public redirects", async () => {
    const requests: Request[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init);
      requests.push(request);
      if (request.url === "https://one.example/start") {
        return new Response(null, { status: 302, headers: { location: "https://two.example/end" } });
      }
      return new Response("done", { headers: { "set-cookie": "credential=must-not-return" } });
    }));
    const gateway = testGateway({ fetch: vi.fn() } as unknown as Fetcher);
    const followed = await gateway.fetch("https://one.example/start", { method: "POST", body: "small" });
    expect(await followed.text()).toBe("done");
    expect(followed.headers.get("set-cookie")).toBeNull();
    expect(requests.map(({ method, url }) => [method, url])).toEqual([
      ["POST", "https://one.example/start"],
      ["GET", "https://two.example/end"],
    ]);

    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, {
      status: 302,
      headers: { location: "http://169.254.169.254/latest" },
    })));
    const deniedRedirect = await gateway.fetch("https://one.example/start");
    expect(deniedRedirect.status).toBe(502);
    expect(await deniedRedirect.json()).toEqual({ error: "redirect_denied" });

  });

  it("streams public responses without an application byte ceiling", async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const upstream = new ReadableStream<Uint8Array>({
      start(value) {
        controller = value;
        value.enqueue(new TextEncoder().encode("first"));
      },
    });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(upstream)));
    const gateway = testGateway({ fetch: vi.fn() } as unknown as Fetcher);

    const response = await gateway.fetch("https://assets.example/compiler");
    const reader = response.body!.getReader();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe("first");
    controller.enqueue(new TextEncoder().encode("second"));
    controller.close();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe("second");
    expect((await reader.read()).done).toBe(true);
  });
});

function testGateway(binding: Fetcher): Fetcher {
  return {
    fetch(input: RequestInfo | URL, init?: RequestInit) {
      return handleManagedEgress(new Request(input, init), binding, SUBJECT);
    },
  } as Fetcher;
}
