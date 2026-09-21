import { describe, expect, it, vi } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { PROVIDER_TELEMETRY_WINDOW_MS, SqliteProviderTelemetryStore, summarizeProviderObservationGroups,
  summarizeProviderObservations, type ProviderObservation } from "../src/provider-telemetry";
import { resolveThreadRoute, ROUTING_CANDIDATES, routingPolicySchema, ThreadRoutePin } from "../src/thread-model-routing";

const now = 10_000_000;
const candidate = ROUTING_CANDIDATES.find(c => c.backend === "openrouter" && c.thinking === "high")!;
const sample = (patch: Partial<ProviderObservation> = {}): ProviderObservation => ({
  timestamp: now - 1_000, source: "probe", workerColo: null, clientIngressColo: null,
  backend: candidate.backend, model: candidate.provider_model, effort: candidate.thinking,
  outcome: "success", status: 200, headersMs: 5, fullResponseMs: 200,
  generationTtftMs: 100, clientDeliveryMs: null, elapsedMs: 200, ...patch,
});
const aggregate = (patch = {}) => ({
  ...summarizeProviderObservationGroups([sample(), sample(), sample()], now)[0], ...patch,
});
const runtime = (metrics: unknown[]) => ({ openrouter: true, vercel: true, cloudflare: true, workerColo: "LHR", provider_performance: metrics });
const answer = (id = candidate.id, confidence = .9) => ({ answers: {
  candidate: { choice: id, confidence }, family: { choice: "terminal", confidence: .95 },
} });
async function route(metrics: unknown[], policy = {}) {
  const ai = { run: vi.fn(async () => answer()) };
  const result = await resolveThreadRoute(ai, "Fix the build", routingPolicySchema.parse(policy), runtime(metrics));
  const state = JSON.parse((ai.run.mock.calls[0] as unknown as [string, {state:string}])[1].state);
  return { result, state };
}

describe("successful generation TTFT and honest deployment scope", () => {
  it("summarizes TTFT separately, counts availability failures and never rewards fast failures", () => {
    const summary = summarizeProviderObservations([
      sample({ timestamp: now - 3000, generationTtftMs: 100 }),
      sample({ timestamp: now - 2000, generationTtftMs: 300 }),
      sample({ generationTtftMs: 200 }),
      sample({ timestamp: now, outcome: "timeout", generationTtftMs: 1, fullResponseMs: 1, status: null }),
      sample({ timestamp: now + 1, generationTtftMs: 1 }),
      sample({ timestamp: now - PROVIDER_TELEMETRY_WINDOW_MS - 1, generationTtftMs: 1 }),
    ], now);
    expect(summary).toMatchObject({ sampleCount: 4, successCount: 3, censoredCount: 1,
      availabilityFailureCount: 1, timeoutCount: 1, generationTtftSampleCount: 3,
      generationTtftP50Ms: 200, generationTtftEwmaMs: 172, fullResponseP50Ms: 200,
      usable: true, lastObservedAt: now, lastTtftObservedAt: now - 1000, ageMs: 0, ttftAgeMs: 1000 });
  });
  it("does not convert HTTP errors, headers, or malformed durations into TTFT", () => {
    const result = summarizeProviderObservations([
      sample({ generationTtftMs: null }), sample({ generationTtftMs: NaN }),
      sample({ generationTtftMs: -1 }), sample({ generationTtftMs: Infinity }),
      sample({ status: 503, generationTtftMs: 1 }),
      sample({ backend: "openrouter", status: null, generationTtftMs: 1 }),
      sample({ backend: "vercel", status: null, generationTtftMs: 1 }),
    ], now);
    expect(result.generationTtftSampleCount).toBe(0);
    expect(result.generationTtftP50Ms).toBeNull();
    expect(result.generationTtftEwmaMs).toBeNull();
    expect(result.availabilityFailureCount).toBe(3);
  });
  it.each(["workers_ai", "cloudflare"])("accepts validated %s generation without inventing an HTTP status", backend => {
    const result = summarizeProviderObservations(Array.from({ length: 3 }, () => sample({ backend, status: null, headersMs: null })), now);
    expect(result).toMatchObject({ successCount: 3, availabilityFailureCount: 0,
      generationTtftSampleCount: 3, generationTtftP50Ms: 100, usable: true });
  });
  it("pools only deployment probe geography and preserves separate live region groups", () => {
    const groups = summarizeProviderObservationGroups([
      sample(), sample({ workerColo: "LHR" }), sample({ workerColo: "SJC" }),
      sample({ source: "live", workerColo: "LHR" }), sample({ source: "live", workerColo: "SJC" }),
    ], now);
    expect(groups).toHaveLength(4);
    expect(groups[0]).toMatchObject({ source: "probe", scope: "deployment_global", workerColo: null, sampleCount: 3 });
    expect(groups.slice(1).map(g => [g.scope, g.workerColo, g.sampleCount])).toEqual([
      ["deployment_global", null, 2], ["worker_colo", "LHR", 1], ["worker_colo", "SJC", 1],
    ]);
  });
  it("atomically supports a full scheduled day within the hard request cap", () => {
    const db = new DatabaseSync(":memory:");
    const sql = { exec(query: string, ...bindings: any[]) {
      const stmt = db.prepare(query);
      return stmt.columns().length ? stmt.all(...bindings) : (stmt.run(...bindings), []);
    } };
    const store = new SqliteProviderTelemetryStore(sql);
    expect(store.reserveProbe("2026-09-21", 4097)).toBe(false);
    for (let i = 0; i < 3168; i++) expect(store.reserveProbe("2026-09-21", 3168)).toBe(true);
    expect(new SqliteProviderTelemetryStore(sql).reserveProbe("2026-09-21", 3168)).toBe(false);
    expect(store.reserveProbe("2026-09-22", 3168)).toBe(true);
    db.close();
  });
});

describe("Jev responsiveness trust boundary", () => {
  it("includes all 57 candidates and their matching global TTFT, without a sixteen-group cutoff", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const metrics = ROUTING_CANDIDATES.map(c => aggregate({ backend: c.backend, model: c.provider_model, effort: c.thinking }));
      const { result, state } = await route(metrics);
      expect(state.candidates).toHaveLength(57);
      expect(result.audit?.provider_telemetry?.provider_performance).toHaveLength(57);
      for (const c of state.candidates) {
        expect(c.responsiveness).toMatchObject({ live: null, probe: {
          generationTtftSampleCount: 3, generationTtftP50Ms: 100,
          ageMs: 1000, ttftAgeMs: 1000, workerColo: null, regionalMatch: false,
        } });
      }
      expect(result.selection).toBe("prior");
      expect(result.estimate).toBeNull();
    } finally { vi.restoreAllMocks(); }
  });
  it("bounds a full live and probe catalog without duplicating telemetry in Jev state", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const metrics = ROUTING_CANDIDATES.flatMap(c => [
        aggregate({ backend: c.backend, model: c.provider_model, effort: c.thinking }),
        aggregate({ backend: c.backend, model: c.provider_model, effort: c.thinking, source: "live", scope: "worker_colo", workerColo: "LHR" }),
      ]);
      const { result, state } = await route(metrics);
      expect(result.audit?.provider_telemetry?.provider_performance).toHaveLength(114);
      expect(state.candidates).toHaveLength(57);
      expect(state.provider_telemetry).not.toHaveProperty("provider_performance");
      expect(JSON.stringify(state).length).toBeLessThan(64_000);
      expect(state.candidates.every((c: any) => c.responsiveness.live && c.responsiveness.probe)).toBe(true);
    } finally { vi.restoreAllMocks(); }
  });
  it("projects only bounded fields and rejects stale, forged regional, invalid, unavailable and conflicting metrics", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const good = aggregate({ prompt: "secret-prompt", apiKey: "secret-key", successRate: 1, arbitrary: { body: "secret-body" } });
      const { result, state } = await route([good]);
      expect(JSON.stringify(result.audit?.provider_telemetry)).not.toMatch(/secret|apiKey|successRate|arbitrary/);
      expect(state.candidates.find((c: any) => c.id === candidate.id).responsiveness.probe).not.toBeNull();
      for (const bad of [
        aggregate({ lastObservedAt: now + 1 }), aggregate({ lastObservedAt: now - PROVIDER_TELEMETRY_WINDOW_MS - 1 }),
        aggregate({ generationTtftP50Ms: Infinity }), aggregate({ generationTtftEwmaMs: -1 }),
        aggregate({ generationTtftSampleCount: 4 }), aggregate({ successCount: 5 }),
        aggregate({ availabilityFailureCount: 1 }), aggregate({ source: "probe", scope: "client_ingress", clientIngressColo: "LHR" }),
        aggregate({ workerColo: "LHR" }), aggregate({ model: "unlisted" }),
        aggregate({ scope: "worker_colo", workerColo: "SJC" }), aggregate({ windowMs: PROVIDER_TELEMETRY_WINDOW_MS + 1 }),
      ]) {
        expect((await route([bad])).result.audit?.provider_telemetry?.provider_performance).toEqual([]);
      }
      expect((await route([good, aggregate({ generationTtftP50Ms: 1 })])).result.audit?.provider_telemetry?.provider_performance).toEqual([]);
      expect((await route([good, good])).result.audit?.provider_telemetry?.provider_performance).toHaveLength(1);
      const unavailable = await resolveThreadRoute({ run: async () => answer() }, "task", routingPolicySchema.parse({}),
        { ...runtime([good]), openrouter: false });
      expect(unavailable.audit?.provider_telemetry?.provider_performance).toEqual([]);
    } finally { vi.restoreAllMocks(); }
  });
  it("treats absent, sparse and stale successful TTFT as unknown even after recent failures", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      for (const metric of [
        aggregate({ generationTtftSampleCount: 2 }),
        aggregate({ lastObservedAt: now, lastTtftObservedAt: now - PROVIDER_TELEMETRY_WINDOW_MS - 1,
          sampleCount: 4, censoredCount: 1, availabilityFailureCount: 1, timeoutCount: 1 }),
        aggregate({ generationTtftSampleCount: 0, lastTtftObservedAt: null, generationTtftP50Ms: null, generationTtftEwmaMs: null }),
      ]) {
        const { result, state } = await route([metric]);
        expect(result.audit?.provider_telemetry?.provider_performance[0]).toMatchObject({ ttftUsable: false, generationTtftP50Ms: null });
        expect(state.candidates.find((c: any) => c.id === candidate.id).responsiveness).toMatchObject({ live: null, probe: null });
      }
      const { state } = await route([]);
      expect(state.candidates.every((c: any) => c.responsiveness.live === null && c.responsiveness.probe === null)).toBe(true);
    } finally { vi.restoreAllMocks(); }
  });
  it("keeps matching live and global probe evidence separate and candidate restrictions authoritative", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const live = aggregate({ source: "live", scope: "worker_colo", workerColo: "LHR", generationTtftP50Ms: 80 });
      const { state } = await route([aggregate(), live], { candidates: [candidate.id] });
      expect(state.candidates).toHaveLength(1);
      expect(state.candidates[0].responsiveness).toMatchObject({
        live: { generationTtftP50Ms: 80, regionalMatch: true },
        probe: { generationTtftP50Ms: 100, regionalMatch: false },
      });
      await expect(route([aggregate()], { candidates: [candidate.id], min_success_rate: .1 })).rejects.toThrow("no route admitted");
      const injected = await resolveThreadRoute({ run: async () => answer() }, { provider_performance: [aggregate()] },
        routingPolicySchema.parse({}), runtime([]));
      expect(injected.audit?.provider_telemetry?.provider_performance).toEqual([]);
    } finally { vi.restoreAllMocks(); }
  });
  it("passes cost and latency signals to the chooser without overriding its answer, confidence or thread pin", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const cheap = ROUTING_CANDIDATES.find(c => c.backend === "openrouter" && c.model === "gpt-5.6-luna" && c.thinking === "high")!;
      const fast = ROUTING_CANDIDATES.find(c => c.backend === "openrouter" && c.model === "gpt-6-astra" && c.thinking === "high")!;
      const metrics = [aggregate({ model: cheap.model, generationTtftP50Ms: 800, generationTtftEwmaMs: 800 }),
        aggregate({ model: fast.model, generationTtftP50Ms: 100, generationTtftEwmaMs: 100 })];
      // Deterministic boundary double: verifies actual candidate inputs, not real Jev semantic accuracy.
      const ai = { run: vi.fn(async (_model: string, input: any) => {
        const state = JSON.parse(input.state);
        expect(input.questions.candidate.instructions).toContain("fast failures are never a latency advantage");
        expect(state.candidates.every((c: any) => c.catalog_price_hint && c.responsiveness.probe)).toBe(true);
        const byDuration = state.preferences.duration > state.preferences.cost;
        const ranked = [...state.candidates].sort((a: any, b: any) => byDuration
          ? a.responsiveness.probe.generationTtftP50Ms - b.responsiveness.probe.generationTtftP50Ms
          : a.catalog_price_hint.input - b.catalog_price_hint.input);
        return answer(ranked[0].id, .6);
      }) };
      const choose = (duration: number, cost: number) => resolveThreadRoute(ai, "Choose for this task",
        routingPolicySchema.parse({ candidates: [cheap.id, fast.id], preferences: { duration, cost } }), runtime(metrics));
      const economical = await choose(1, 90), responsive = await choose(90, 1);
      expect(economical.audit?.candidate_choice).toBe(cheap.id);
      expect(responsive.audit?.candidate_choice).toBe(fast.id);
      expect(responsive.audit?.candidate_confidence).toBe(.6);
      expect(responsive.selection).toBe("fallback");
      expect(responsive.estimate).toBeNull();
      let saved: typeof responsive | undefined;
      const pin = new ThreadRoutePin({ read: () => saved, commit: x => { saved = x; } });
      await pin.resolve(async () => responsive);
      expect(await pin.resolve(async () => economical)).toBe(responsive);
      expect(ai.run).toHaveBeenCalledTimes(2);
    } finally { vi.restoreAllMocks(); }
  });
});


it("keeps the root provider/model/effort after latency reverses, including durable reload; new threads use fresh measurements", async () => {
  vi.spyOn(Date, "now").mockReturnValue(now);
  const db = new DatabaseSync(":memory:");
  try {
    db.exec("CREATE TABLE route (singleton INTEGER PRIMARY KEY, value TEXT NOT NULL)");
    const store = {
      read: () => {
        const row = db.prepare("SELECT value FROM route WHERE singleton = 1").get() as { value: string } | undefined;
        return row ? JSON.parse(row.value) : undefined;
      },
      commit: (value: Awaited<ReturnType<typeof resolveThreadRoute>>) => {
        db.prepare("INSERT INTO route VALUES (1, ?)").run(JSON.stringify(value));
      },
    };
    const first = ROUTING_CANDIDATES.find(c => c.backend === "openrouter" && c.model === "gpt-5.6-sol" && c.thinking === "high")!;
    const later = ROUTING_CANDIDATES.find(c => c.backend === "vercel" && c.model === "gpt-6-astra" && c.thinking === "low")!;
    let fastest = first.id;
    const ai = { run: vi.fn(async (_model: string, input: any) => {
      const candidates = JSON.parse(input.state).candidates;
      candidates.sort((a: any, b: any) => a.responsiveness.probe.generationTtftP50Ms - b.responsiveness.probe.generationTtftP50Ms);
      return answer(candidates[0].id);
    }) };
    const choose = () => resolveThreadRoute(ai, "Continue the task", routingPolicySchema.parse({ candidates: [first.id, later.id] }),
      runtime([first, later].map(c => aggregate({ backend: c.backend, model: c.provider_model, effort: c.thinking,
        generationTtftP50Ms: c.id === fastest ? 100 : 8000, generationTtftEwmaMs: c.id === fastest ? 100 : 8000 }))));
    const pin = new ThreadRoutePin(store);
    const initial = await pin.resolve(choose);
    expect(initial).toMatchObject({ backend: first.backend, model: first.model, thinking: first.thinking });
    fastest = later.id;
    expect(await pin.resolve(choose)).toEqual(initial);
    expect(await new ThreadRoutePin(store).resolve(choose)).toEqual(initial);
    expect(ai.run).toHaveBeenCalledOnce();
    // A new root or child can use the new measurements without changing the retained root.
    expect(await choose()).toMatchObject({ backend: later.backend, model: later.model, thinking: later.thinking });
    expect(store.read()).toEqual(initial);
    expect(ai.run).toHaveBeenCalledTimes(2);
  } finally { db.close(); vi.restoreAllMocks(); }
});

describe("trusted client ingress cohorts", () => {
  it("conditions live samples on ingress without inventing execution and keeps probes global", () => {
    const samples = [sample({ source: "live", clientIngressColo: "ATH", generationTtftMs: 10 }),
      sample({ source: "live", clientIngressColo: "SJC", generationTtftMs: 100 }),
      sample({ source: "probe", clientIngressColo: "ATH", workerColo: "LHR" })];
    const groups = summarizeProviderObservationGroups(samples, now, { clientIngressColo: "ATH" });
    expect(groups).toHaveLength(3);
    expect(groups.find(x => x.scope === "client_ingress")).toMatchObject({ source: "live", workerColo: null,
      clientIngressColo: "ATH", sampleCount: 1, generationTtftP50Ms: 10 });
    expect(groups.find(x => x.scope === "deployment_global" && x.source === "live")).toMatchObject({
      clientIngressColo: null, workerColo: null, sampleCount: 2, generationTtftP50Ms: 55, generationTtftP95Ms: 100 });
    expect(groups.find(x => x.source === "probe")).toMatchObject({ scope: "deployment_global", clientIngressColo: null, workerColo: null });
    expect(summarizeProviderObservationGroups(samples, now, {}).every(x => x.scope === "deployment_global")).toBe(true);
  });
  it("sends different matched live TTFT to Jev by ingress, preserves exact effort and retains pins", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const a = ROUTING_CANDIDATES.find(c => c.backend === "cloudflare" && c.model === "gpt-6-astra" && c.thinking === "low")!;
      const b = ROUTING_CANDIDATES.find(c => c.backend === "vercel" && c.model === "gpt-5.6-sol" && c.thinking === "high")!;
      const samples = ["ATH", "SJC"].flatMap(clientIngressColo => [a, b].flatMap(c => Array.from({ length: 3 }, () => sample({
        source: "live", clientIngressColo, backend: c.backend, model: c.provider_model, effort: c.thinking,
        generationTtftMs: (clientIngressColo === "ATH") === (c.id === a.id) ? 20 : 180,
      }))));
      // This chooser asserts the trust boundary; it is not evidence of real Jev accuracy.
      const ai = { run: vi.fn(async (_: string, input: any) => {
        const state = JSON.parse(input.state);
        expect(state.provider_telemetry.workerColo).toBeNull();
        for (const c of state.candidates) expect(c.responsiveness.live).toMatchObject({
          workerColo: null, regionalMatch: true, regionalMatchKind: "client_ingress", generationTtftSampleCount: 3,
        });
        return answer([...state.candidates].sort((x, y) => x.responsiveness.live.generationTtftP50Ms - y.responsiveness.live.generationTtftP50Ms)[0].id);
      }) };
      const choose = (clientIngressColo: string) => resolveThreadRoute(ai, "task", routingPolicySchema.parse({ candidates: [a.id, b.id] }), {
        openrouter: true, vercel: true, cloudflare: true, workerColo: null, clientIngressColo,
        provider_performance: summarizeProviderObservationGroups(samples, now, { clientIngressColo }),
      });
      const ath = await choose("ATH"), sjc = await choose("SJC");
      expect(ath).toMatchObject({ backend: a.backend, model: a.model, provider_model: a.provider_model, thinking: a.thinking });
      expect(sjc).toMatchObject({ backend: b.backend, model: b.model, provider_model: b.provider_model, thinking: b.thinking });
      let saved: typeof ath | undefined;
      await new ThreadRoutePin({ read: () => saved, commit: x => { saved = x; } }).resolve(async () => ath);
      expect(await new ThreadRoutePin({ read: () => saved, commit: x => { saved = x; } }).resolve(() => choose("SJC"))).toBe(ath);
      expect(ai.run).toHaveBeenCalledTimes(2);
    } finally { vi.restoreAllMocks(); }
  });
  it("falls back to global live TTFT for unknown, sparse or stale ingress without claiming a regional match", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const base = Array.from({ length: 3 }, () => sample({ source: "live", clientIngressColo: "SJC", generationTtftMs: 120 }));
      for (const extra of [[], [sample({ source: "live", clientIngressColo: "ATH", generationTtftMs: 20 })],
        Array.from({ length: 3 }, () => sample({ source: "live", clientIngressColo: "ATH", timestamp: now - PROVIDER_TELEMETRY_WINDOW_MS - 1 }))]) {
        const ai = { run: vi.fn(async () => answer()) };
        const result = await resolveThreadRoute(ai, "task", routingPolicySchema.parse({ candidates: [candidate.id] }), {
          ...runtime(summarizeProviderObservationGroups([...base, ...extra], now, { clientIngressColo: "ATH" })), workerColo: null, clientIngressColo: "ATH",
        });
        expect(result.audit?.provider_telemetry?.provider_performance).toHaveLength(1);
        expect(result.audit?.provider_telemetry?.provider_performance[0]).toMatchObject({ source: "live", scope: "deployment_global",
          regionalMatch: false, regionalMatchKind: null, generationTtftP50Ms: 120, ttftUsable: true });
      }
    } finally { vi.restoreAllMocks(); }
  });
  it("rejects mismatched ingress, probe regional claims and conflicting regional aggregates while retaining global fallback", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const global = aggregate({ source: "live" });
      const regional = aggregate({ source: "live", scope: "client_ingress", clientIngressColo: "ATH" });
      for (const bad of [regional, { ...regional, clientIngressColo: "LHR", workerColo: "LHR" },
        { ...regional, clientIngressColo: "LHR", source: "probe" }]) {
        expect((await route([bad])).result.audit?.provider_telemetry?.provider_performance).toEqual([]);
      }
      const ai = { run: vi.fn(async () => answer()) };
      const result = await resolveThreadRoute(ai, "task", routingPolicySchema.parse({}), {
        ...runtime([global, regional, { ...regional, generationTtftP50Ms: 80 }]), clientIngressColo: "ATH",
      });
      expect(result.audit?.provider_telemetry?.provider_performance).toHaveLength(1);
      expect(result.audit?.provider_telemetry?.provider_performance[0]).toMatchObject({ scope: "deployment_global", regionalMatch: false });
    } finally { vi.restoreAllMocks(); }
  });
});
