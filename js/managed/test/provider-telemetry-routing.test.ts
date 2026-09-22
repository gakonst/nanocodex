import { describe, expect, it, vi } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { PROVIDER_TELEMETRY_WINDOW_MS, SqliteProviderTelemetryStore, summarizeProviderObservationGroups,
  summarizeProviderObservations, type ProviderObservation } from "../src/provider-telemetry";
import { resolveThreadRoute, ROUTING_CANDIDATES, routingPolicySchema } from "../src/thread-model-routing";

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
});


describe("routing is independent of provider telemetry", () => {
  it("sends the same compact Jev input regardless of geography or probe history", async () => {
    const ai = { run: vi.fn(async (_model: string, _input: unknown) => answer()) };
    const policy = routingPolicySchema.parse({});
    const clean = await resolveThreadRoute(ai, "Fix the build", policy, runtime([]));
    const metrics = ROUTING_CANDIDATES.map(c => aggregate({ backend: c.backend, model: c.provider_model,
      effort: c.thinking, prompt: "private-prompt", apiKey: "private-key" }));
    const noisy = await resolveThreadRoute(ai, "Fix the build", policy, { ...runtime(metrics), workerColo: "SJC", clientIngressColo: "ATH" });
    expect(ai.run.mock.calls[1][1]).toEqual(ai.run.mock.calls[0][1]);
    expect(noisy.audit).toEqual(clean.audit);
    expect(noisy.audit).not.toHaveProperty("provider_telemetry");
    expect(JSON.stringify(ai.run.mock.calls)).not.toMatch(/private-key|private-prompt|generationTtft|workerColo|clientIngressColo/);
    const input = ai.run.mock.calls[0][1] as { state: string; questions: { candidate: { criteria: object } } };
    expect(Object.keys(input.questions.candidate.criteria)).toHaveLength(67);
    expect(Buffer.byteLength(JSON.stringify(input))).toBeLessThan(16_000);
    const state = JSON.parse(input.state);
    expect(Object.keys(state.model_profiles)).toHaveLength(7);
    expect(Object.keys(state.effort_profiles)).toHaveLength(3);
    expect(state).toHaveProperty("eval_evidence");
    expect(state).not.toHaveProperty("candidates");
    expect(clean.estimate).toBeNull();
  });
});
