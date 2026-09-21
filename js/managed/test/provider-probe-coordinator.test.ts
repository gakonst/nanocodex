import { createExecutionContext, env, runInDurableObject, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type Env } from "../src/index";
import type { ProviderProbeCoordinator } from "./provider-probe-worker";

const namespace = () => (env as unknown as { NANOCODEX_PROVIDER_PROBE_COORDINATOR: DurableObjectNamespace<ProviderProbeCoordinator> }).NANOCODEX_PROVIDER_PROBE_COORDINATOR;
describe("real workerd probe coordination", () => {
  it("runs the scheduled entrypoint once per slot, persists its budget, and returns content-free aggregates", async () => {
    const stub = namespace().getByName(crypto.randomUUID());
    const now = Date.now();
    const runtime = { ...env, NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => stub } } as unknown as Env;
    const ctx = createExecutionContext();
    worker.scheduled({ scheduledTime: now, cron: "*/30 * * * *" } as ScheduledController, runtime, ctx);
    await waitOnExecutionContext(ctx);
    expect(await stub.tick(now)).toBe(0);
    const snapshot = await stub.snapshot();
    expect(snapshot).toHaveLength(3);
    for (const metric of snapshot) expect(metric).toMatchObject({ source: "probe", scope: "deployment_global",
      workerColo: null, backend: "workers_ai", sampleCount: 1, successCount: 1, generationTtftSampleCount: 1 });
    expect(JSON.stringify(snapshot)).not.toMatch(/Reply with|\"content\"|\"key\"/);
    await runInDurableObject(stub, async (_instance, state) => {
      expect(state.storage.sql.exec("SELECT count FROM provider_probe_budget").one().count).toBe(3);
      expect(state.storage.sql.exec("SELECT COUNT(*) AS n FROM provider_probe_ticks").one().n).toBe(1);
      // Exhaust the remaining durable budget without issuing paid requests.
      state.storage.sql.exec("UPDATE provider_probe_budget SET count=144");
    });
    // A subsequent valid slot cannot bypass the day's durable cap.
    const next = Math.floor(Date.now() / 1_800_000) * 1_800_000 + 1_800_000;
    expect(await stub.tick(next)).toBe(0);
    expect(await stub.snapshot()).toHaveLength(3);
  });
  it("disabled scheduling does not contact the coordinator", async () => {
    let calls = 0;
    const runtime = { NANOCODEX_PROVIDER_PROBES: "false", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName() { calls++; throw Error("unexpected"); } } } as unknown as Env;
    const ctx = createExecutionContext();
    worker.scheduled({ scheduledTime: Date.now() } as ScheduledController, runtime, ctx);
    await waitOnExecutionContext(ctx);
    expect(calls).toBe(0);
  });
});

it("private observe validates content-free live data and snapshot filters trusted ingress cohorts", async () => {
  const stub = namespace().getByName(crypto.randomUUID());
  const sample = { timestamp: Date.now(), source: "live", workerColo: null, clientIngressColo: "ATH",
    backend: "cloudflare", model: "openai/gpt-6-astra", effort: "high", outcome: "success", status: 200,
    headersMs: 5, fullResponseMs: 40, generationTtftMs: 20, clientDeliveryMs: null, elapsedMs: 40 };
  for (let i = 0; i < 3; i++) expect(await stub.observe({ ...sample, ...(i === 1 ? { model: "gpt-6-astra" } : {}), prompt: "synthetic-private-prompt", key: "synthetic-secret" })).toBe(true);
  expect(await stub.observe({ ...sample, clientIngressColo: "SJC", generationTtftMs: 30 })).toBe(true);
  for (const patch of [{ source: "probe" }, { timestamp: Date.now() + 60_000 }, { generationTtftMs: -1 },
    { clientIngressColo: "forged-long-colo" }, { status: 500 }, { model: "a".repeat(257) }, { model: "synthetic@example.invalid" }, { effort: "max" }]) {
    expect(await stub.observe({ ...sample, ...patch })).toBe(false);
  }
  const ath = await stub.snapshot({ clientIngressColo: "ATH" });
  expect(ath).toHaveLength(2);
  expect(ath.find(x => x.scope === "client_ingress")).toMatchObject({ workerColo: null, clientIngressColo: "ATH",
    sampleCount: 3, generationTtftSampleCount: 3, generationTtftP50Ms: 20, generationTtftP95Ms: 20 });
  expect(ath.find(x => x.scope === "deployment_global")).toMatchObject({ source: "live", sampleCount: 4, workerColo: null, clientIngressColo: null });
  expect((await stub.snapshot({}))).toHaveLength(1);
  expect((await stub.snapshot())).toHaveLength(1);
  expect((await stub.snapshot({ clientIngressColo: "SJC" })).find(x => x.scope === "client_ingress")).toMatchObject({ sampleCount: 1 });
  expect(JSON.stringify(ath)).not.toMatch(/synthetic|prompt|secret/);
  await runInDurableObject(stub, async (_instance, state) => {
    expect(state.storage.sql.exec("SELECT COUNT(*) AS n FROM provider_observations").one().n).toBe(4);
    const persisted = [...state.storage.sql.exec("SELECT sample FROM provider_observations")];
    expect(JSON.stringify(persisted)).not.toMatch(/synthetic|prompt|secret/);
  });
});
