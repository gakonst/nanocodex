import assert from "node:assert/strict";
import test from "node:test";
import { open } from "../managed/Agent.mjs";
import { normalizeObservationFrame, normalizeObservationSurfaces } from "nanocodex-tools/observation";

const surface = { id: "screen", name: "Screen", kind: "desktop", source: "account", machine_id: "laptop", machine_name: "Laptop", route_token: "lease:1" };
const frame = { captured_at: 123, width: 1, height: 1, mime_type: "image/png", data: "AAAA" };
const options = { baseUrl: "https://managed.test", apiKey: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}` };
const id = "11111111-1111-7111-8111-111111111111";

test("public SDK requests exact hand scope/generation and stops on consumer break", async () => {
  const requests = [];
  const agent = open(id, { ...options, fetch: async (url, init) => {
    requests.push({ url: new URL(url), init });
    return Response.json(new URL(url).pathname.endsWith("/hands") ? { surfaces: [surface] } : { status: "frame", frame });
  } });
  const [screen] = await agent.hands.list();
  for await (const result of agent.hands.frames(screen)) { assert.deepEqual(result.frame, frame); break; }
  assert.equal(requests.length, 2);
  assert.equal(requests[1].url.searchParams.get("source"), "account");
  assert.equal(requests[1].url.searchParams.get("route_token"), "lease:1");
  assert.equal(requests[1].url.searchParams.has("api_key"), false);
  assert.match(new Headers(requests[1].init.headers).get("authorization"), /^Bearer /);
});

test("viewer abort interrupts an outstanding frame and stale routes do not switch", async () => {
  const controller = new AbortController();
  let calls = 0;
  const agent = open(id, { ...options, fetch: async (_url, init) => {
    calls++;
    controller.abort();
    init.signal.throwIfAborted();
  } });
  await assert.rejects(agent.hands.frames(surface, { signal: controller.signal }).next());
  assert.equal(calls, 1);
  const stale = open(id, { ...options, fetch: async () => { calls++; return Response.json({ error: "observation_unavailable" }, { status: 409 }); } });
  await assert.rejects(stale.hands.frames(surface).next());
  assert.equal(calls, 2);
});

test("screen protocol bounds dimensions, formats, payloads and identities", () => {
  assert.deepEqual(normalizeObservationFrame(frame), frame);
  for (const invalid of [{ width: 0 }, { height: 9000 }, { mime_type: "image/svg+xml" }, { data: "https://example.test/x.png" }, { data: "a".repeat(240004) }, { data: "A===" }, { extra: true }]) {
    assert.throws(() => normalizeObservationFrame({ ...frame, ...invalid }));
  }
  assert.throws(() => normalizeObservationSurfaces([{ id: "bad/id", name: "Screen", kind: "desktop" }]));
  assert.throws(() => normalizeObservationSurfaces(Array(9).fill({ id: "x", name: "X", kind: "phone" })));
});
