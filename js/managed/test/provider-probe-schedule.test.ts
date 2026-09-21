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
  it("adds twelve frontier probes only with the explicit gate and binding", () => {
    const env = { AI: { run: async () => ({}) }, OPENROUTER_API_KEY: "fixture-openrouter", AI_GATEWAY_API_KEY: "fixture-vercel", NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "true" };
    const targets = configuredProbeTargets(env);
    expect(targets).toHaveLength(45);
    expect(new Set(targets.map(t => JSON.stringify([t.backend,t.model,t.effort]))).size).toBe(45);
    expect(targets.filter(t => t.backend === "cloudflare")).toHaveLength(12);
    expect(targets.filter(t => t.backend === "cloudflare").every(t => t.model.startsWith("openai/") && !t.key && !t.accountId)).toBe(true);
    expect(configuredProbeTargets({...env,AI:undefined}).some(t => t.backend === "cloudflare")).toBe(false);
    for (const value of [undefined,"false","TRUE","1"]) expect(configuredProbeTargets({...env,NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED:value}).some(t => t.backend === "cloudflare")).toBe(false);
  });
  it("schedules frontier REST probes without an AI binding and preserves the exact deployment credentials", () => {
    const env = { NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "true", CLOUDFLARE_AI_API_TOKEN: "fixture-cloudflare-token",
      NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "0123456789abcdef0123456789abcdef" };
    const targets = configuredProbeTargets(env);
    expect(targets).toHaveLength(12);
    expect(targets.every(t => t.backend === "cloudflare" && t.model.startsWith("openai/")
      && t.key === env.CLOUDFLARE_AI_API_TOKEN && t.accountId === env.NANOCODEX_CLOUDFLARE_ACCOUNT_ID)).toBe(true);
    expect(configuredProbeTargets({ ...env, AI: { run: async () => ({}) } }).filter(t => t.backend === "cloudflare")).toEqual(targets);
    expect(configuredProbeTargets({ ...env, NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "false" })).toEqual([]);
  });
  it("partial or invalid REST configuration disables frontier probes even with a native binding", () => {
    const env = { AI: { run: async () => ({}) }, NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "true",
      CLOUDFLARE_AI_API_TOKEN: "fixture-cloudflare-token", NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "0123456789abcdef0123456789abcdef" };
    for (const invalid of [{ CLOUDFLARE_AI_API_TOKEN: undefined }, { NANOCODEX_CLOUDFLARE_ACCOUNT_ID: undefined },
      { CLOUDFLARE_AI_API_TOKEN: "" }, { CLOUDFLARE_AI_API_TOKEN: " " }, { CLOUDFLARE_AI_API_TOKEN: "bad\r\nheader" },
      { NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "" }, { NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "../other-account" },
      { NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "https://evil.example" }, { NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "0".repeat(31) }]) {
      const targets = configuredProbeTargets({ ...env, ...invalid });
      expect(targets.some(t => t.backend === "cloudflare")).toBe(false);
      expect(targets.filter(t => t.backend === "workers_ai")).toHaveLength(3);
    }
  });
  it.each(["0", "-1", "4097", "NaN", "1.2", ""])("invalid request budget %s disables spend", limit => {
    expect(probeDailyLimit({ NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT: limit })).toBe(0);
  });
});
