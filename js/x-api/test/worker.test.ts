import { SELF } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { browseX, parseXRequest } from "nanocodex-tools/x";

const post = (id: string, text = "Public post") => ({
  id, text, author: { id: "user-1", name: "Example", screen_name: "example" },
  url: `https://x.com/example/status/${id}`,
});
const fetcher = vi.fn<typeof fetch>();
let expectedRequests = 0;
function reply(path: string, status: number, body: unknown, options?: { headers: Record<string, string> }) {
  expectedRequests++;
  fetcher.mockImplementationOnce(async (input, init) => {
    expect(String(input)).toBe(path.startsWith('https:') ? path : `https://api.fxtwitter.com${path}`);
    expect(new Headers(init?.headers).has('authorization')).toBe(false);
    expect(new Headers(init?.headers).has('cookie')).toBe(false);
    expect(init?.redirect).toBe('manual');
    return new Response(typeof body === 'string' ? body : JSON.stringify(body), { status, headers: options?.headers });
  });
}
const request = (path: string) => SELF.fetch(`https://x.internal${path}`);
const tool = () => browseX({ fetch: (input, init) => SELF.fetch(String(input), init) });
const context = () => ({ callId: "call", parentCallId: "", sessionId: "session", model: "test", signal: new AbortController().signal });

beforeEach(() => {
  expectedRequests = 0;
  fetcher.mockReset();
  fetcher.mockRejectedValue(new Error('Unexpected upstream request'));
  vi.stubGlobal('fetch', fetcher);
});
afterEach(() => {
  expect(fetcher).toHaveBeenCalledTimes(expectedRequests);
  vi.unstubAllGlobals();
});

describe("native X tool through the Worker", () => {
  it("converts public posts with quote and video sources through first-party HTTP", async () => {
    reply("/2/status/1001", 200, { status: {
      ...post("1001"), quote: post("1002", "Quoted post"),
      media: { videos: [{ type: "video", url: "https://video.twimg.com/example.mp4", duration: 2 }] },
    } });
    const result = await tool().handler({ action: "post", url: "https://twitter.com/example/status/1001?s=20", thread: "off", nocache: true }, context()) as any;
    expect(result).toMatchObject({ url: "https://x.com/example/status/1001", source: "fxtwitter", postCount: 1, cache: "bypass" });
    expect(result.markdown).toContain("https://x.com/example/status/1002");
    expect(result.markdown).toContain("https://video.twimg.com/example.mp4");
    expect(result.posts[0].media.videos[0].duration_ms).toBe(2000);
  });

  it("preserves focal post and context ordering when bounding a thread", async () => {
    reply("/2/thread/1012", 200, { thread: [post("1010"), post("1011"), post("1012"), post("1013")] });
    const result = await tool().handler({ action: "post", url: "https://x.com/example/status/1012", thread: "2", replies: "off", nocache: true }, context()) as any;
    expect(result.posts.map((p: any) => p.id)).toEqual(["1011", "1012"]);
    expect(result.posts.map((p: any) => p.context)).toEqual(["parent", "post"]);
    expect(result.warnings).toEqual(["Thread truncated to 2 posts."]);
  });

  it("falls back to syndication on a public post outage", async () => {
    reply("/2/status/1020", 502, "unavailable");
    reply("https://cdn.syndication.twimg.com/tweet-result?id=1020&lang=en&token=0", 200, {
      id_str: "1020", text: "Fallback post", user: { screen_name: "example", name: "Example" },
    });
    const response = await request("/example/status/1020?format=json&thread=off&nocache=true");
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ source: "syndication", postCount: 1, warnings: [expect.stringContaining("fallback")] });
  });

  it.each(["PRIVATE_TWEET", "NOT_FOUND"])("handles provider status %s without inventing content", async (message) => {
    reply("/2/status/1030", 200, { code: message === "PRIVATE_TWEET" ? 403 : 404, message });
    if (message === "NOT_FOUND") {
      reply("https://cdn.syndication.twimg.com/tweet-result?id=1030&lang=en&token=0", 404, "missing");
    }
    const response = await request("/example/status/1030?thread=off&nocache=true");
    expect(response.status).toBe(404);
  });

  it("lists profile original posts and keeps the opaque continuation", async () => {
    reply("/2/profile/example", 200, { user: { screen_name: "example", name: "Example", followers: 42 } });
    reply("/2/profile/example/statuses?count=20&with_replies=false", 200, {
      results: [post("1040"), { ...post("1041"), replying_to_status: ["1040"] }, { ...post("1042"), reposted_by: { screen_name: "another" } }],
      cursor: { bottom: "opaque+/cursor=" },
    });
    const result = await tool().handler({ action: "profile", handle: "example", full: true, nocache: true }, context()) as any;
    expect(result.posts.map((p: any) => p.id)).toEqual(["1040"]);
    expect(result).toMatchObject({ nextCursor: "opaque+/cursor=", profile: { followers: 42 } });
  });

  it.each(["followers", "following"] as const)("reads %s with cursor pagination", async (action) => {
    reply(`/2/profile/example/${action}?cursor=opaque%2B%2Fcursor%3D&count=2`, 200, {
      results: [{ name: "Next", screen_name: "next" }], cursor: { bottom: "next-cursor" },
    });
    const result = await tool().handler({ action, handle: "example", cursor: "opaque+/cursor=", limit: 2, nocache: true }, context()) as any;
    expect(result).toMatchObject({ resource: action, nextCursor: "next-cursor", users: [{ screen_name: "next" }] });
  });

  it("searches with encoded queries and reports outages as retryable", async () => {
    reply("/2/search?q=from%3Aexample+%26+workers&feed=top&count=2", 200, { results: [post("1050")] });
    const result = await tool().handler({ action: "search", q: "from:example & workers", feed: "top", limit: 2, nocache: true }, context()) as any;
    expect(result.posts[0].id).toBe("1050");
    reply("/2/search?q=outage&feed=latest&count=20", 200, { code: 404, results: [] });
    expect(await tool().handler({ action: "search", q: "outage", nocache: true }, context())).toMatchObject({
      status: "unavailable", http_status: 503, retry_after: 30, error: { code: "search_unavailable" },
    });
  });

  it("preserves rate limits without retrying or caching failures", async () => {
    reply("/2/status/1060", 429, "rate limited", { headers: { "retry-after": "75" } });
    const response = await request("/example/status/1060?thread=off&format=json");
    expect(response.status).toBe(429);
    expect(response.headers.get("retry-after")).toBe("75");
    expect(await response.json()).toMatchObject({ retry_after: 75, error: { code: "rate_limited" } });
    reply("/2/status/1060", 200, { status: post("1060") });
    expect((await request("/example/status/1060?thread=off&format=json")).status).toBe(200);
  });

  it("caches successful public results for repeat requests and supports bypass", async () => {
    const path = "/example/status/1070?thread=off&format=json";
    reply("/2/status/1070", 200, { status: post("1070", "Cached") });
    expect((await request(path)).headers.get("x-cache")).toBe("MISS");
    expect((await request(path)).headers.get("x-cache")).toBe("HIT");
    reply("/2/status/1070", 200, { status: post("1070", "Fresh") });
    const fresh = await request(`${path}&nocache=true`);
    expect(fresh.headers.get("x-cache")).toBe("BYPASS");
    expect((await fresh.json<any>()).posts[0].text).toBe("Fresh");
    expect((await (await request(path)).json<any>()).posts[0].text).toBe("Cached");
  });

  it("serves Markdown by default and HEAD without a body", async () => {
    reply("/2/status/1080", 200, { status: post("1080") });
    const response = await request("/example/status/1080?thread=off");
    expect(response.headers.get("content-type")).toContain("text/markdown");
    expect(await response.text()).toContain("Public post");
    const head = await SELF.fetch("https://x.internal/example/status/1080?thread=off", { method: "HEAD" });
    expect(head.status).toBe(200);
    expect(await head.text()).toBe("");
  });

  it("classifies malformed upstream data as provider failure", async () => {
    reply("/2/profile/example/followers?count=20", 200, "<html>outage</html>");
    const response = await request("/example/followers?nocache=true");
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { code: "invalid_response" } });
  });

  it("rejects upstream redirects without following them", async () => {
    reply("/2/profile/example/following?count=20", 302, "", { headers: { location: "https://example.com/redirected" } });
    expect((await request("/example/following?nocache=true")).status).toBe(502);
  });

  it.each([
    "/api/convert?url=https://example.com/example/status/1001",
    "/api/convert?url=https://x.com@example.com/example/status/1001",
    "/api/browse?resource=search&q=x&limit=21",
    "/api/browse?resource=followers&handle=example&page=11",
    "/api/browse?resource=search&q=x&feed=users",
    "/api/browse?resource=search&q=x&q=y",
    "/api/browse?resource=search&q=x&authorization=secret",
    "/search?q=x&action=profile",
    "/example/status/1001?thread=2oops",
  ])("rejects invalid requests before provider access: %s", async (path) => {
    expect((await request(path)).status).toBe(400);
  });

  it("rejects unsupported methods, routes, and cancelled tool calls", async () => {
    expect((await SELF.fetch("https://x.internal/api/convert", { method: "POST" })).status).toBe(405);
    expect((await request("/api/unknown")).status).toBe(404);
    const controller = new AbortController();
    const reason = new Error("turn cancelled");
    controller.abort(reason);
    await expect(tool().handler({ action: "profile", handle: "example" }, { ...context(), signal: controller.signal })).rejects.toBe(reason);
    expect(() => parseXRequest({ action: "profile", handle: "example", q: "ignored query" })).toThrow();
  });
});
