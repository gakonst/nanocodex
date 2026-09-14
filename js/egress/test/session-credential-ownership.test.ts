import { describe, expect, it, vi } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";

const subject = `managed-session-v1_${"a".repeat(64)}`;
const request = (value = subject) => new Request("https://public-egress.internal/v1/request", {
  headers: { "x-nanocodex-subject": value, "x-nanocodex-target-url": "https://example.com/" },
});

describe("versioned managed credential routing", () => {
  it("resolves a new subject from its Session without allocating a directory object", async () => {
    const direct = vi.fn(async (input: RequestInfo | URL) => {
      expect(String(input)).toBe(`https://managed-ownership.internal/v1/resolve?subject=${subject}`);
      return Response.json({ user_id: "managed-owner" });
    });
    const directory = vi.fn(() => { throw new Error("must not allocate a subject DO"); });
    const upstream = vi.fn(async () => new Response("ok"));
    const response = await handleEgress(request(), {
      MANAGED_AGENT_OWNERSHIP: { fetch: direct }, AGENT_SUBJECTS: { getByName: directory },
    } as unknown as EgressEnv, undefined, upstream as typeof fetch);
    expect(response.status).toBe(200);
    expect(direct).toHaveBeenCalledTimes(1);
    expect(directory).not.toHaveBeenCalled();
    expect(upstream).toHaveBeenCalledTimes(1);
  });

  for (const failure of [404, 410, 503, "throw", "missing"] as const) {
    it(`does not resurrect a direct subject through legacy fallback after ${failure}`, async () => {
      const directory = vi.fn(() => ({ fetch: async () => Response.json({ user_id: "stale-owner" }) }));
      const direct = vi.fn(async () => {
        if (failure === "throw") throw new Error("ownership unavailable");
        return new Response(null, { status: typeof failure === "number" ? failure : 503 });
      });
      const upstream = vi.fn(async () => new Response("must not forward"));
      const response = await handleEgress(request(), {
        ...(failure === "missing" ? {} : { MANAGED_AGENT_OWNERSHIP: { fetch: direct } }),
        AGENT_SUBJECTS: { getByName: directory },
      } as unknown as EgressEnv, undefined, upstream as typeof fetch);
      expect(response.status).not.toBe(200);
      expect(directory).not.toHaveBeenCalled();
      expect(upstream).not.toHaveBeenCalled();
    });
  }

  for (const legacy of ["A".repeat(43), "b".repeat(64)]) {
    it(`retains legacy directory authority for ${legacy.length}-character subjects`, async () => {
      const directory = vi.fn(() => ({ fetch: async () => Response.json({ user_id: "legacy-owner" }) }));
      const direct = vi.fn(async () => { throw new Error("legacy must not use Session authority"); });
      const response = await handleEgress(request(legacy), {
        MANAGED_AGENT_OWNERSHIP: { fetch: direct }, AGENT_SUBJECTS: { getByName: directory },
      } as unknown as EgressEnv, undefined, (async () => new Response("ok")) as typeof fetch);
      expect(response.status).toBe(200);
      expect(directory).toHaveBeenCalledWith(`agent-subject-v1:${legacy}`);
      expect(direct).not.toHaveBeenCalled();
    });
  }

  it("rejects malformed reserved subjects and legacy bind/unbind for direct subjects", async () => {
    const directory = vi.fn(() => { throw new Error("must not allocate a subject DO"); });
    const direct = vi.fn(async () => { throw new Error("malformed subject"); });
    const env = {
      MANAGED_AGENT_OWNERSHIP: { fetch: direct }, AGENT_SUBJECTS: { getByName: directory },
    } as unknown as EgressEnv;
    expect((await handleEgress(request(`managed-session-v1_${"z".repeat(64)}`), env)).status).toBe(403);
    for (const method of ["PUT", "DELETE"]) {
      expect((await handleEgress(new Request(`https://broker.internal/subjects/${subject}`, {
        method, headers: { "content-type": "application/json" },
        body: JSON.stringify({ user_id: "managed-owner" }),
      }), env)).status).toBe(403);
    }
    expect(directory).not.toHaveBeenCalled();
    expect(direct).not.toHaveBeenCalled();
  });
});
