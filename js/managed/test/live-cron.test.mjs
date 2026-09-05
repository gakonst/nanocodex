import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";
import { Agent } from "../../nanocodex/managed/index.mjs";

// Production only: an isolated synthetic agent, the public SDK used by the UI,
// and a real wall-clock Durable Object alarm. No direct prompt submission.
test("production cron CRUD survives reconnect and dispatches one completed turn", { timeout: 10 * 60_000 }, async (t) => {
  const apiKey = process.env.NANOCODEX_CRON_TEST_API_KEY;
  assert.ok(apiKey, "NANOCODEX_CRON_TEST_API_KEY is required for live cron evidence");
  const baseUrl = "https://nanocodex.gakonst.workers.dev";
  const options = { apiKey, baseUrl, fetch: (url, init) => fetch(url, {
    ...init, signal: init?.signal ? AbortSignal.any([init.signal, AbortSignal.timeout(15_000)]) : AbortSignal.timeout(15_000),
  }) };
  const agent = await Agent.create({ ...options, settings: {
    model: "gpt-5.6-luna", thinking: "low", reasoningMode: "standard", fastMode: false,
  } });
  const id = `cron-ci-${randomUUID()}`;
  // Always remove our schedule and agent, including when any assertion fails.
  t.after(async () => {
    try { await agent.triggers.delete(id); }
    finally { await agent.delete(); }
  });
  t.diagnostic(`synthetic agent ${agent.id}; deployment ${process.env.GITHUB_SHA ?? "local"}`);
  const marker = `CRON_OK_${randomUUID().replaceAll("-", "")}`;
  const config = { cron: "0 9 * * *", timezone: "Europe/Athens", input: "Unused paused draft", enabled: false };
  const paused = await agent.triggers.put(id, config);
  assert.equal(paused.next_run_at, null);
  assert.equal(paused.enabled, false);
  const reconnected = Agent.open(agent.id, options);
  assert.deepEqual(await reconnected.triggers.get(id), paused);
  assert.ok((await reconnected.triggers.list()).some((row) => row.id === id));
  assert.deepEqual(await reconnected.triggers.put(id, config), paused, "idempotent save must preserve state");
  const activeConfig = { ...config, cron: "* * * * *", input: `Reply with exactly ${marker}. Do not use tools.`, enabled: true };
  const enabled = await reconnected.triggers.put(id, activeConfig);
  assert.ok(enabled.next_run_at > Date.now() - 5_000);
  assert.ok(enabled.next_run_at <= Date.now() + 60_000);
  assert.equal(enabled.input, activeConfig.input);
  t.diagnostic("create, reconnect, list, idempotent save, edit and resume passed; awaiting real alarm");
  let fired;
  const alarmDeadline = Date.now() + 3 * 60_000;
  while (Date.now() < alarmDeadline) {
    fired = await reconnected.triggers.get(id);
    if (fired.last_turn_id) break;
    await delay(1_000);
  }
  assert.match(fired.last_turn_id ?? "", /^cron:/, "wall-clock alarm must admit a durable turn");
  assert.equal(fired.last_run_at, enabled.next_run_at);
  assert.ok(fired.next_run_at > fired.last_run_at);
  const disabled = await reconnected.triggers.put(id, { ...activeConfig, enabled: false });
  assert.equal(disabled.enabled, false);
  assert.equal(disabled.next_run_at, null);
  const pausedAt = Date.now();
  t.diagnostic(`real cron alarm admitted ${fired.last_turn_id}; paused future occurrences`);
  let turn;
  const turnDeadline = Date.now() + 6 * 60_000;
  while (Date.now() < turnDeadline) {
    const response = await options.fetch(`${baseUrl}/v1/agents/${agent.id}/turns/${encodeURIComponent(fired.last_turn_id)}`, {
      headers: { authorization: `Bearer ${apiKey}` },
    });
    assert.equal(response.status, 200, "scheduled turn receipt must be readable");
    turn = await response.json();
    if (["completed", "failed", "cancelled"].includes(turn.state)) break;
    await delay(1_000);
  }
  assert.equal(turn.state, "completed", "scheduled model response must complete, not merely be queued");
  assert.ok(turn.terminal.final_message.includes(marker), "scheduled turn must execute the edited prompt");
  // Cross another real minute boundary to prove pause persists beyond reconnect.
  await delay(Math.max(0, pausedAt + 65_000 - Date.now()));
  const afterPause = await Agent.open(agent.id, options).triggers.get(id);
  assert.equal(afterPause.last_turn_id, fired.last_turn_id);
  assert.equal(afterPause.next_run_at, null);
  assert.equal((await agent.state()).accepted_turns, 1, "no duplicate dispatch or dispatch while paused");
  await reconnected.triggers.delete(id);
  await reconnected.triggers.delete(id);
  assert.equal((await Agent.open(agent.id, options).triggers.list()).length, 0);
  await assert.rejects(reconnected.triggers.get(id), (error) => error.status === 404);
  t.diagnostic("PASS: real scheduled response completed; pause, no duplicates, and idempotent deletion verified");
});
