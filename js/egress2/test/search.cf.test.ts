import { describe, expect, it, vi } from "vitest";
import { createSearchHandler } from "../src/search";

const url = "https://nanocodex.internal/v1/search";
const body = JSON.stringify({ session_id: "session-one", model: "gpt-6-sol", commands: { search_query: [{ q: "read-only query" }] } });
const request = (overrides: RequestInit = {}) => new Request(url, { method: "POST", body,
  headers: { "x-managed2-owner": "owner", authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL" }, ...overrides });

describe("private Egress2 search", () => {
  it("routes fixed API-key endpoint with only synthesized auth/settings, bounded output and fixed timing", async () => {
    const upstream = vi.fn(async (sent: Request, owner: string) => {
      expect(owner).toBe("owner");
      expect(sent.url).toBe("https://api.openai.com/v1/alpha/search");
      expect(sent.headers.get("authorization")).toBe("Bearer sk-test-only");
      expect(sent.headers.has("x-managed2-owner")).toBe(false);
      expect(sent.redirect).toBe("manual");
      expect(await sent.json()).toEqual({ id: "session-one", model: "gpt-6-sol",
        commands: { search_query: [{ q: "read-only query" }] },
        settings: { allowed_callers: ["direct"], external_web_access: true }, max_output_tokens: 10_000 });
      return Response.json({ output: "Found [source](https://example.org)", provider_secret: "do-not-return" },
        { headers: { "server-timing": "untrusted;dur=111" } });
    });
    const handler = createSearchHandler({ readCredential: async () => ({ kind: "openai" as const, secret: "sk-test-only" }), upstreamFetch: upstream, log: () => {} });
    const response = await handler(request(), {});
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ output: "Found [source](https://example.org)" });
    expect(response.headers.get("server-timing")).toContain("search_upstream;dur=");
    expect(response.headers.get("server-timing")).not.toContain("untrusted");
    expect(upstream).toHaveBeenCalledTimes(1);
  });

  it("routes subscription with its own account metadata, and refuses redirects and non-JSON results", async () => {
    const seen: Request[] = [];
    const handler = createSearchHandler({ readCredential: async () => ({ kind: "chatgpt" as const, secret: "synthetic", accountId: "account", fedramp: true, expiresAt: Date.now() + 100_000 }),
      upstreamFetch: async (sent: Request) => { seen.push(sent); return new Response(null, { status: 302, headers: { location: "https://evil.example" } }); }, log: () => {} });
    const response = await handler(request(), {});
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({ error: "search_unavailable" });
    expect(seen[0].url).toBe("https://chatgpt.com/backend-api/codex/alpha/search");
    expect(seen[0].headers.get("chatgpt-account-id")).toBe("account");
    expect(seen[0].headers.get("x-openai-fedramp")).toBe("true");
    const malformed = createSearchHandler({ readCredential: async () => ({ kind: "openai" as const, secret: "x" }),
      upstreamFetch: async () => Response.json({ output: { unexpected: true } }), log: () => {} });
    expect((await malformed(request(), {})).status).toBe(502);
  });

  it("rejects untrusted routes, methods, owner/placeholder and malformed or oversized requests before credential lookup", async () => {
    const readCredential = vi.fn(async () => ({ kind: "openai" as const, secret: "x" }));
    const upstreamFetch = vi.fn(async () => Response.json({ output: "ok" }));
    const handler = createSearchHandler({ readCredential, upstreamFetch, log: () => {} });
    expect((await handler(new Request("https://nanocodex.internal/v1/other", request()), {})).status).toBe(403);
    expect((await handler(request({ method: "PUT" }), {})).status).toBe(405);
    expect((await handler(request({ headers: { "x-managed2-owner": "owner", authorization: "Bearer arbitrary" } }), {})).status).toBe(403);
    expect((await handler(request({ body: "not json" }), {})).status).toBe(400);
    expect((await handler(request({ body: "x".repeat(65_537) }), {})).status).toBe(400);
    expect(readCredential).not.toHaveBeenCalled();
    expect(upstreamFetch).not.toHaveBeenCalled();
  });
});
