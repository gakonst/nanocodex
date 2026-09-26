import { env, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";

// Reproducible *fixture* latency probe through the real Worker, DO, WASM and
// Egress2 layers. The synthetic provider/tool are deliberately fixed at a
// 2500-ms search; these measurements are not live-provider or production data.
it("records paired synchronous/async source-turn and terminal-uptake latency", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const durations: { mode: "sync" | "async"; source_ms: number; terminal_ms: number;
    stages_ms?: Record<string, number> }[] = [];
  for (const mode of ["sync", "async", "async", "sync", "sync", "async"] as const) {
    const started = performance.now();
    const created = await SELF.fetch("https://api.test/v1/agents", {
      method: "POST", headers: { authorization, "content-type": "application/json" },
      body: JSON.stringify({ input: "Use async web__run once, then report its result.", async_tools: mode === "async" }),
    });
    expect(created.status).toBe(202);
    const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
    const turn = () => SELF.fetch(`https://api.test/v1/agents/${agentId}/turns/${turnId}`, {
      headers: { authorization },
    }).then(response => response.json<{ state: string; message?: string }>());
    await expect.poll(turn, { timeout: 15_000, interval: 75 }).toMatchObject({ state: "completed",
      message: mode === "async" ? "Waiting for background search" : expect.stringContaining("[async fixture]"),
    });
    const sourceMs = performance.now() - started;
    let stages: Record<string, number> | undefined;
    if (mode === "async") {
      const [job] = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
        headers: { authorization },
      })).json<{ job_id: string }[]>();
      expect(job).toBeDefined();
      stages = {};
      const deadline = performance.now() + 15_000;
      while (performance.now() < deadline) {
        const { state } = await (await SELF.fetch(
          `https://api.test/v1/agents/${agentId}/jobs/${job!.job_id}`, { headers: { authorization } },
        )).json<{ state: string }>();
        stages[state] ??= Math.round(performance.now() - started);
        if (state === "delivered") break;
        await new Promise(resolve => setTimeout(resolve, 75));
      }
      expect(stages.delivered).toBeDefined();
    }
    durations.push({ mode, source_ms: Math.round(sourceMs), terminal_ms: Math.round(performance.now() - started),
      ...(stages ? { stages_ms: stages } : {}) });
  }
  const median = (values: number[]) => values.sort((a, b) => a - b)[1]!;
  const syncSource = median(durations.filter(row => row.mode === "sync").map(row => row.source_ms));
  const asyncSource = median(durations.filter(row => row.mode === "async").map(row => row.source_ms));
  const asyncTerminal = median(durations.filter(row => row.mode === "async").map(row => row.terminal_ms));
  console.log(JSON.stringify({ fixture: "2500ms-search",
    transport: (env as unknown as { RESPONSES_TRANSPORT: string }).RESPONSES_TRANSPORT,
    n_per_mode: 3, durations, sync_source_median_ms: syncSource,
    async_source_median_ms: asyncSource, async_terminal_median_ms: asyncTerminal }));
  expect(syncSource).toBeGreaterThan(2000);
  expect(asyncSource).toBeLessThan(syncSource);
}, 90_000);
