import { describe, expect, it, vi } from "vitest";
import { MeetingPreviewRuntime, routeMeetingPreview, generateMeetingSummary, MEETING_PREVIEW_INTERVAL_MS, MEETING_PREVIEW_MAX_DELTA, MEETING_PREVIEW_MAX_PENDING,
  type MeetingPreviewEnv } from "../src/meeting-preview";
import type { Principal } from "../src/account-auth";
import type { InferenceSessionEnv } from "../src/inference-session";
import { routeInferenceApi } from "../src/inference-api";

const capture = "a745f840-f68d-46ce-9d70-5daf9693a582";
const identity = { "x-meeting-owner": "11111111-1111-4111-8111-111111111111", "x-meeting-organization": "organization",
  "x-meeting-team": "team", "x-meeting-epoch": "1" };
const principal: Principal = { kind: "api_key", userId: identity["x-meeting-owner"], organizationId: "organization", teamId: "team",
  authorizationEpoch: 1, role: "owner", subjectId: `user:${identity["x-meeting-owner"]}`, credentialId: "test",
  capabilities: ["agents:read", "agents:write", "tools:use"] };
function fixture() {
  const persisted = new Map<string, unknown>();
  let now = 100_000;
  const generate = vi.fn(async (_previous: string, _pending: string) => "First recap");
  const storage = {
    async get<T>(key: string) { return structuredClone(persisted.get(key)) as T | undefined; },
    async put(key: string, value: unknown) { persisted.set(key, structuredClone(value)); },
    async list<T>({ prefix }: { prefix: string }) {
      return new Map([...persisted.entries()].filter(([key]) => key.startsWith(prefix))) as Map<string, T>;
    },
    async delete(key: string) { persisted.delete(key); },
  } as unknown as DurableObjectStorage;
  const runtime = new MeetingPreviewRuntime(storage, generate, () => now);
  const request = (method: string, body?: unknown, headers: Record<string, string> = identity, id = capture) => runtime.fetch(new Request(
    `https://meeting.internal/${id}/preview`, { method, headers, ...(body === undefined ? {} : { body: JSON.stringify(body) }) }));
  return { request, generate, persisted, tick: (ms: number) => { now += ms; } };
}

describe("account meeting preview", () => {
  it("retains ordered finalized deltas, recaps once and replays exact revisions without spending twice", async () => {
    const f = fixture();
    const first = await f.request("POST", { revision: 1, delta: "Speaker A confirms the roadmap and action items for next week." });
    expect(first.status).toBe(200);
    expect(await first.json()).toMatchObject({ revision: 1, summary: "", summary_revision: 0, status: "pending" });
    const second = await f.request("POST", { revision: 2, delta: " Speaker B owns the launch checklist and will report on Monday." });
    expect(await second.json()).toMatchObject({ revision: 2, summary: "First recap", summary_revision: 2, status: "updated" });
    expect(f.generate).toHaveBeenCalledTimes(1);
    expect(f.generate.mock.calls[0]?.[1]).toContain("launch checklist");
    expect(await (await f.request("POST", { revision: 2, delta: " Speaker B owns the launch checklist and will report on Monday." })).json())
      .toMatchObject({ revision: 2, status: "unchanged", summary_revision: 2 });
    expect(f.generate).toHaveBeenCalledTimes(1);
    expect((await f.request("POST", { revision: 2, delta: "different" })).status).toBe(409);
    expect(await (await f.request("POST", { revision: 4, delta: "future" })).json())
      .toMatchObject({ error: "revision_gap", expected_revision: 3 });
    expect(await (await f.request("GET")).json()).toMatchObject({ revision: 2, summary_revision: 2, summary: "First recap" });
    f.tick(MEETING_PREVIEW_INTERVAL_MS - 1);
    expect(await (await f.request("POST", { revision: 3, delta: " The decision remains open pending a budget review." })).json())
      .toMatchObject({ summary_revision: 2, status: "pending" });
    f.tick(1);
    expect(await (await f.request("POST", { revision: 4, delta: " Owner C will decide the budget." })).json())
      .toMatchObject({ summary_revision: 4, status: "updated" });
    expect(f.generate).toHaveBeenCalledTimes(2);
    expect(f.generate.mock.calls[1]?.[0]).toBe("First recap");
    expect(f.generate.mock.calls[1]?.[1]).not.toContain("launch checklist");
  });
  it("generates via the stateless low-effort GLM route without agent turns or a classifier", async () => {
    const run = vi.fn(async (_model: string, _input: unknown) => ({ choices: [{ message: { content: "Decision: launch Monday." }, finish_reason: "stop" }] }));
    const summary = await generateMeetingSummary({ AI: { run } } as unknown as InferenceSessionEnv,
      "Prior recap", "Decision: launch Monday.", new AbortController().signal);
    expect(summary).toBe("Decision: launch Monday.");
    expect(run).toHaveBeenCalledTimes(1);
    expect(run.mock.calls[0]?.[0]).toBe("@cf/zai-org/glm-5.3");
    const input = run.mock.calls[0]?.[1] as unknown as { reasoning_effort: string; max_completion_tokens: number;
      messages: Array<{ role: string; content: string }> };
    expect(input.reasoning_effort).toBe("low");
    expect(input.max_completion_tokens).toBe(320);
    expect(input.messages.some(message => message.role === "system" && message.content.includes("untrusted data"))).toBe(true);
    expect(input.messages.some(message => message.role === "user" && message.content.includes("Prior recap"))).toBe(true);
  });
  it("enforces owner and epoch, size limits, deletion tombstones", async () => {
    const f = fixture();
    expect((await f.request("POST", { revision: 1, delta: "x".repeat(MEETING_PREVIEW_MAX_DELTA + 1) })).status).toBe(400);
    expect((await f.request("POST", { revision: 1, delta: "A short segment." })).status).toBe(200);
    expect((await f.request("GET", undefined, { ...identity, "x-meeting-epoch": "2" })).status).toBe(404);
    expect((await f.request("POST", { revision: 2, delta: "other" }, { ...identity, "x-meeting-owner": "other" })).status).toBe(404);
    expect((await f.request("DELETE")).status).toBe(204);
    expect((await f.request("GET")).status).toBe(404);
    expect((await f.request("POST", { revision: 2, delta: "late" })).status).toBe(410);
  });
  it("commits deltas before generation, retains failed context for next attempt, caps account spend", async () => {
    const f = fixture();
    f.generate.mockRejectedValueOnce(Error("synthetic provider failure"));
    expect(await (await f.request("POST", { revision: 1, delta: "Decision: postpone launch to Monday due to testing.".repeat(2) })).json())
      .toMatchObject({ revision: 1, summary_revision: 0, status: "unavailable" });
    expect(await (await f.request("GET")).json()).toMatchObject({ revision: 1, summary_revision: 0 });
    expect((f.persisted.get(`capture:${capture}`) as { pending: string }).pending).toContain("postpone launch");
    f.tick(MEETING_PREVIEW_INTERVAL_MS);
    expect(await (await f.request("POST", { revision: 2, delta: " QA agrees to retest the release." })).json())
      .toMatchObject({ revision: 2, summary_revision: 2, status: "updated" });
    expect(f.generate.mock.calls[1]?.[1]).toContain("postpone launch");
    f.tick(MEETING_PREVIEW_INTERVAL_MS);
    f.persisted.set("budget", { day: 0, minute: 3, day_count: 120, minute_count: 0 });
    expect(await (await f.request("POST", { revision: 3, delta: " Planning notes and decisions for the team are finalized.".repeat(2) })).json())
      .toMatchObject({ revision: 3, summary_revision: 2, status: "pending", retry_after_seconds: 86210 }); // UTC-day quota
    expect(f.generate).toHaveBeenCalledTimes(2);
  });
  it("caps pending UTF-8 text including the server-added separator and closes at the TTL boundary", async () => {
    const f = fixture();
    f.generate.mockRejectedValue(Error("provider down"));
    const delta = "x".repeat(MEETING_PREVIEW_MAX_DELTA);
    for (let revision = 1; revision <= 5; revision++)
      expect((await f.request("POST", { revision, delta })).status).toBe(200);
    expect((f.persisted.get(`capture:${capture}`) as { pending: string }).pending.length).toBe(5 * (MEETING_PREVIEW_MAX_DELTA + 1));
    expect((await f.request("POST", { revision: 6, delta })).status).toBe(413);
    expect((f.persisted.get(`capture:${capture}`) as { pending: string }).pending.length).toBeLessThan(MEETING_PREVIEW_MAX_PENDING);
    f.tick(3 * 60 * 60 * 1000);
    expect((await f.request("GET")).status).toBe(404);
    expect((await f.request("POST", { revision: 6, delta: "late" })).status).toBe(410);
    expect((f.persisted.get(`capture:${capture}`) as { pending: string; summary: string }).pending).toBe("");
  });
  it("shares its two-per-minute attempt ceiling across captures and never charges exact replays", async () => {
    const f = fixture();
    const ids = [capture, "b745f840-f68d-46ce-9d70-5daf9693a582", "c745f840-f68d-46ce-9d70-5daf9693a582"];
    for (const id of ids) expect((await f.request("POST", { revision: 1, delta: "Decision: continue testing the launch next week. ".repeat(2) }, identity, id)).status).toBe(200);
    expect(f.generate).toHaveBeenCalledTimes(2);
    expect((await f.request("POST", { revision: 1, delta: "Decision: continue testing the launch next week. ".repeat(2) }, identity, ids[2])).status).toBe(200);
    expect(f.generate).toHaveBeenCalledTimes(2);
    f.tick(60_000);
    expect(await (await f.request("POST", { revision: 2, delta: " Further actions were approved." }, identity, ids[2])).json())
      .toMatchObject({ status: "updated", summary_revision: 2 });
    expect(f.generate).toHaveBeenCalledTimes(3);
  });
  it("bounds active meeting previews and daily creation even for tiny no-inference deltas", async () => {
    const f = fixture();
    const ids = Array.from({ length: 61 }, (_, i) => `a745f840-f68d-46ce-9d70-${String(i).padStart(12, "0")}`);
    for (const id of ids.slice(0, 8))
      expect((await f.request("POST", { revision: 1, delta: "hi" }, identity, id)).status).toBe(200);
    expect((await f.request("POST", { revision: 1, delta: "hi" }, identity, ids[8])).status).toBe(429);
    expect((await f.request("DELETE", undefined, identity, ids[0])).status).toBe(204);
    for (const id of ids.slice(8, 60)) {
      expect((await f.request("POST", { revision: 1, delta: "hi" }, identity, id)).status).toBe(200);
      expect((await f.request("DELETE", undefined, identity, id)).status).toBe(204);
    }
    expect((await f.request("POST", { revision: 1, delta: "hi" }, identity, ids[60])).status).toBe(429);
    expect(f.generate).not.toHaveBeenCalled();
  });
  it("serializes concurrent requests during a slow provider and commits before it resolves", async () => {
    const f = fixture();
    let release!: (value: string) => void;
    f.generate.mockImplementationOnce(() => new Promise<string>(resolve => { release = resolve; }));
    const underway = f.request("POST", { revision: 1, delta: "Decision: continue testing the launch next week. ".repeat(2) });
    for (let i = 0; i < 20 && !release; i++) await new Promise(resolve => setTimeout(resolve, 0));
    expect(release).toBeDefined();
    const busy = await f.request("GET");
    expect(busy.status).toBe(409);
    expect(busy.headers.get("retry-after")).toBe("1");
    expect((f.persisted.get(`capture:${capture}`) as { revision: number }).revision).toBe(1);
    release("First recap");
    expect(await (await underway).json()).toMatchObject({ status: "updated", summary_revision: 1 });
  });
  it("fails closed while disabled and fences inference-key credentials before account authorization", async () => {
    const url = new URL(`https://example.test/v1/meetings/${capture}/preview`);
    const request = new Request(url, { headers: { authorization: `Bearer nci_live_${"a".repeat(43)}_${"b".repeat(43)}` } });
    expect((await routeInferenceApi(request, {} as Parameters<typeof routeInferenceApi>[1], url))?.status).toBe(403);
    expect((await routeMeetingPreview(new Request(url), {} as MeetingPreviewEnv, url))?.status).toBe(503);
  });
  it("rejects spoofed authority and enforces origin/permissions before a DO dispatch", async () => {
    const fetch = vi.fn(async (_request: Request) => Response.json({ status: "pending" }));
    const env = { AI: { run: vi.fn() }, NANOCODEX_MEETING_PREVIEW_ENABLED: "true",
      NANOCODEX_MEETING_PREVIEWS: { getByName: () => ({ fetch }) } } as unknown as MeetingPreviewEnv;
    const url = new URL(`https://example.test/v1/meetings/${capture.toUpperCase()}/preview`);
    const call = (method: string, user: Principal = principal, origin?: string) => routeMeetingPreview(
      new Request(url, { method, headers: origin ? { origin } : {}, ...(method === "POST" ? { body: '{"revision":1,"delta":"A meeting"}' } : {}) }),
      env, url, user);
    expect((await call("POST"))?.status).toBe(200);
    expect(fetch.mock.calls[0]?.[0]?.url).toContain(capture); // normalized capture ID
    expect((await call("POST", { ...principal, kind: "account_session" }))?.status).toBe(403);
    expect((await call("POST", { ...principal, kind: "account_session" }, url.origin))?.status).toBe(200);
    expect((await call("GET", { ...principal, capabilities: ["agents:read"] }))?.status).toBe(403);
    expect((await call("GET", { ...principal, kind: "connect_grant" }))?.status).toBe(403);
    expect((await call("GET", { ...principal, kind: "api_key", connectGrant: { grantId: "synthetic", connectors: [], mcpIds: [] } }))?.status).toBe(403);
    expect(fetch).toHaveBeenCalledTimes(2);
  });
});
