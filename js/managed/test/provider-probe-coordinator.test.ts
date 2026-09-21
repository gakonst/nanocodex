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
