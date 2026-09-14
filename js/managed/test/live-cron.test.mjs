import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";
import { Agent } from "../../nanocodex/managed/index.mjs";

// Production only: an isolated synthetic agent, the public SDK used by the UI,
// and a real wall-clock Durable Object alarm. No direct prompt submission.
test("production cron creates isolated sessions and also continues existing conversations", { timeout: 10 * 60_000 }, async (t) => {
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
  const children = new Set();
  // Always remove our schedule and agent, including when any assertion fails.
  t.after(async () => {
    try { await agent.triggers.delete(id); }
    finally {
      await agent.delete();
      for (const child of children) await Agent.open(child, options).delete();
    }
  });
  t.diagnostic(`synthetic agent ${agent.id}; deployment ${process.env.GITHUB_SHA ?? "local"}`);
  const marker = `CRON_OK_${randomUUID().replaceAll("-", "")}`;
  const config = { cron: "0 9 * * *", timezone: "Europe/Athens", input: "Unused paused draft", enabled: false };
  const paused = await agent.triggers.put(id, config);
  assert.equal(paused.next_run_at, null);
  assert.equal(paused.enabled, false);
  assert.equal(paused.session_mode, "new", "new schedules default to fresh sessions");
  const reconnected = Agent.open(agent.id, options);
  assert.deepEqual(await reconnected.triggers.get(id), paused);
  assert.ok((await reconnected.triggers.list()).some((row) => row.id === id));
  assert.deepEqual(await reconnected.triggers.put(id, config), paused, "idempotent save must preserve state");
  let lastFired;
  let pausedAt;
  for (const [index, session_mode] of ["new", "new", "continue"].entries()) {
    const runMarker = `${marker}_${index}`;
    const activeConfig = { ...config, session_mode, cron: "* * * * *", input: `Reply with exactly ${runMarker}. Do not use tools.`, enabled: true };
    const enabled = await reconnected.triggers.put(id, activeConfig);
    assert.ok(enabled.next_run_at > Date.now() - 5_000);
    assert.ok(enabled.next_run_at <= Date.now() + 60_000);
    assert.equal(enabled.input, activeConfig.input);
    t.diagnostic(`${session_mode} run ${index + 1}: saved and reconnected; awaiting real alarm`);
    let fired;
    const alarmDeadline = Date.now() + 3 * 60_000;
    while (Date.now() < alarmDeadline) {
      fired = await reconnected.triggers.get(id);
      if (fired.last_turn_id && fired.last_turn_id !== lastFired?.last_turn_id) break;
      await delay(1_000);
    }
    assert.match(fired.last_turn_id ?? "", /^cron:/, "wall-clock alarm must admit a durable turn");
    assert.equal(fired.last_run_at, enabled.next_run_at);
    assert.ok(fired.next_run_at > fired.last_run_at);
    if (session_mode === "new") {
      assert.notEqual(fired.last_agent_id, agent.id, "fresh run must not advance the source conversation");
      assert.ok(fired.last_agent_id);
      assert.ok(!children.has(fired.last_agent_id), "each occurrence needs a distinct session");
      children.add(fired.last_agent_id);
    } else assert.equal(fired.last_agent_id, agent.id, "continue mode must target the original conversation");
    const disabled = await reconnected.triggers.put(id, { ...activeConfig, enabled: false });
    assert.equal(disabled.enabled, false);
    assert.equal(disabled.next_run_at, null);
    assert.equal(disabled.session_mode, session_mode);
    pausedAt = Date.now();
    let turn;
    const turnDeadline = Date.now() + 6 * 60_000;
    while (Date.now() < turnDeadline) {
      const response = await options.fetch(`${baseUrl}/v1/agents/${fired.last_agent_id}/turns/${encodeURIComponent(fired.last_turn_id)}`, {
        headers: { authorization: `Bearer ${apiKey}` },
      });
      assert.equal(response.status, 200, "scheduled turn receipt must be readable by the owner");
      turn = await response.json();
      if (["completed", "failed", "cancelled"].includes(turn.state)) break;
      await delay(1_000);
    }
    assert.equal(turn.state, "completed", "scheduled model response must complete, not merely be queued");
    assert.ok(turn.terminal.final_message.includes(runMarker), "scheduled turn must execute the edited prompt");
    const target = Agent.open(fired.last_agent_id, options);
    assert.equal((await target.state()).accepted_turns, 1, "fresh sessions have exactly one turn and no copied history");
    assert.deepEqual(await target.settings.read(), await agent.settings.read(), "fresh run inherits model settings");
    assert.equal((await agent.state()).accepted_turns, session_mode === "new" ? 0 : 1);
    if (session_mode === "new") {
      assert.equal((await target.triggers.list()).length, 0, "schedules must not copy recursively");
      assert.ok((await Agent.list(options)).some((entry) => entry.id === target.id), "new run appears in the owner's agent list");
    }
    t.diagnostic(`PASS: ${session_mode} run completed in ${fired.last_agent_id}; turn ${fired.last_turn_id}`);
    lastFired = fired;
  }
  // Cross another real minute boundary to prove pause persists beyond reconnect.
  await delay(Math.max(0, pausedAt + 65_000 - Date.now()));
  const afterPause = await Agent.open(agent.id, options).triggers.get(id);
  assert.equal(afterPause.last_turn_id, lastFired.last_turn_id);
  assert.equal(afterPause.next_run_at, null);
  assert.equal((await agent.state()).accepted_turns, 1, "no duplicate dispatch or dispatch while paused");
  for (const child of children) assert.equal((await Agent.open(child, options).state()).accepted_turns, 1);
  await reconnected.triggers.delete(id);
  await reconnected.triggers.delete(id);
  assert.equal((await Agent.open(agent.id, options).triggers.list()).length, 0);
  await assert.rejects(reconnected.triggers.get(id), (error) => error.status === 404);
  t.diagnostic("PASS: two distinct fresh sessions plus continued conversation completed; settings, ownership list, pause, no duplicates, and deletion verified");
});
