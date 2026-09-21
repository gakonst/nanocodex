import { describe, expect, it } from "vitest";
import { configuredProbeTargets, probeDailyLimit, PROBE_INTERVAL_MS, PROBE_SCHEDULE } from "../src/provider-probe-schedule";

describe("deployment probe schedule", () => {
  it("covers every configured API model and effort without a global subscription identity", () => {
    const targets = configuredProbeTargets({ AI: { run: async () => ({}) }, OPENROUTER_API_KEY: "fixture-openrouter", AI_GATEWAY_API_KEY: "fixture-vercel" });
    expect(targets).toHaveLength(33);
    expect(new Set(targets.map(t => JSON.stringify([t.backend, t.model, t.effort]))).size).toBe(33);
    expect(targets.filter(t => t.backend === "workers_ai")).toHaveLength(3);
    expect(targets.filter(t => t.backend === "openrouter")).toHaveLength(15);
    expect(targets.filter(t => t.backend === "vercel")).toHaveLength(15);
    expect(configuredProbeTargets({})).toEqual([]);
    expect(configuredProbeTargets({ OPENROUTER_API_KEY: " " })).toEqual([]);
    expect(PROBE_SCHEDULE).toBe("*/30 * * * *");
    expect(24 * 60 * 60_000 / PROBE_INTERVAL_MS * targets.length).toBeLessThanOrEqual(probeDailyLimit({}));
  });
  it.each(["0", "-1", "4097", "NaN", "1.2", ""])("invalid request budget %s disables spend", limit => {
    expect(probeDailyLimit({ NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT: limit })).toBe(0);
  });
});
