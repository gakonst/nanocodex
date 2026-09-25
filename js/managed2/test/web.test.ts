import { expect, it, vi } from "vitest";
import { managedWeb } from "../src/web";
import type { ToolContext } from "nanocodex";

it("reports per-call observer phases and Egress2 timings by ToolContext.callId, including failures", async () => {
  const observed: Array<[string, string, number]> = [];
  const fetch = vi.fn(async (request: Request) => {
    expect(request.url).toBe("https://nanocodex.internal/v1/search");
    expect(request.headers.get("x-managed2-owner")).toBe("owner");
    expect(request.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    const input = await request.json() as { commands: unknown; session_id: string };
    expect(input.commands).toEqual({ search_query: [{ q: "example" }] });
    expect(input.session_id).toBe("session");
    return Response.json({ output: "[source](https://example.org)" }, { headers: {
      "server-timing": "search_prepare;dur=1.1, search_credential;dur=2.2, search_upstream;dur=3.3, search_parse;dur=4.4, untrusted;dur=999",
    } });
  });
  const tool = managedWeb({ egress: { fetch } as unknown as Fetcher, owner: "owner",
    onTiming: (ctx, phase, duration) => { observed.push([ctx.callId, phase, duration]); throw new Error("observer is best effort"); } });
  const context = (callId: string): ToolContext => ({ callId, parentCallId: "parent", sessionId: "session", model: "gpt-6-sol", signal: new AbortController().signal });
  expect(await tool.handler({ search_query: [{ q: "example" }] }, context("call-a"))).toBe("[source](https://example.org)");
  await expect(tool.handler({ search_query: "bad" }, context("call-b"))).rejects.toThrow();
  expect(observed.filter(([id]) => id === "call-a").map(([, phase]) => phase)).toEqual([
    "preparation", "egress_dispatch", "parse", "egress_prepare", "egress_credential", "egress_upstream", "egress_parse",
  ]);
  expect(observed.filter(([id]) => id === "call-b").map(([, phase]) => phase)).toEqual(["preparation"]);
  expect(observed.every(([, , duration]) => Number.isFinite(duration) && duration >= 0)).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(1);
});
