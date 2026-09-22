import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";
import { Agent } from "../../nanocodex/managed/index.mjs";

// Production evidence only; run after deploying the goal implementation.
test("production goal completes through real tools and survives pause, reconnect, and resume", { timeout: 8 * 60_000 }, async (t) => {
  const apiKey = process.env.NANOCODEX_GOALS_TEST_API_KEY;
  assert.ok(apiKey, "NANOCODEX_GOALS_TEST_API_KEY is required for live goal evidence");
  const options = { apiKey, baseUrl: "https://nanocodex.gakonst.workers.dev", fetch: (url, init) => fetch(url, {
    ...init, signal: init?.signal ? AbortSignal.any([init.signal, AbortSignal.timeout(15_000)]) : AbortSignal.timeout(15_000),
  }) };
  const agent = await Agent.create({ ...options, settings: {
    model: "gpt-6-luna", thinking: "low", reasoningMode: "standard", fastMode: false,
  } });
  // Never list or delete unrelated account resources, even on failure.
  t.after(async () => { await agent.delete(); });
  t.diagnostic(`synthetic goal agent ${agent.id}; deployment ${process.env.GITHUB_SHA ?? "local"}`);
  const marker = `GOAL_SMOKE_${randomUUID().replaceAll("-", "")}`;
  const objective = `Call get_goal to read this goal. Using exec_command in /brain, write exactly ${marker} followed by a newline to /brain/outputs/goal-smoke.txt (create the directory if needed). Then use a separate exec_command to read that file, print its contents, and verify its contents equal ${marker} with a shell test that exits nonzero on mismatch. Only after that verification call update_goal with status complete. On resumption repeat the read and verification before completing. Use only goal tools and exec_command; do not delegate, use memory, access accounts, or contact external services. Keep the response brief.`;
  const command = async (handle, input) => handle.turn.prompt({ id: randomUUID(), input }).result({ signal: AbortSignal.timeout(30_000) });
  const status = async (handle) => (await command(handle, "/goal status")).finalMessage;
  const waitComplete = async (handle) => {
    const deadline = Date.now() + 3 * 60_000;
    let text;
    do {
      text = await status(handle);
      if (text.startsWith("Goal complete:") && (await handle.state()).active_turns.length === 0) return text;
      assert.ok(!/^Goal (blocked|paused|budgetLimited|usageLimited):/.test(text), "goal stopped before completing");
      await delay(1_000);
    } while (Date.now() < deadline);
    assert.fail("goal did not complete within three minutes");
  };
  const resultsSince = async (handle, after = "0") => {
    const results = [];
    for (;;) {
      const page = await handle.events.page({ after, limit: 256 });
      for (const entry of page.data) {
        const event = entry.data.event;
        if (entry.type === "event" && event?.type === "tool.result") results.push(event.payload);
      }
      if (!page.hasMore) return { results, cursor: page.data.at(-1)?.cursor ?? after };
      assert.ok(page.data.length, "history pagination must advance");
      after = page.data.at(-1).cursor;
    }
  };
  const goalEvidence = (results) => {
    const reads = results.filter((row) => row.tool === "get_goal" && row.status === "completed");
    const completions = results.filter((row) => row.tool === "update_goal" && row.status === "completed"
      && row.structured_result?.goal?.status === "complete");
    assert.ok(reads.some((row) => row.structured_result?.goal?.objective === objective), "get_goal must execute and return the persisted objective");
    assert.ok(completions.length, "update_goal must execute successfully and return complete; prose is not evidence");
    const goal = completions.at(-1).structured_result.goal;
    assert.equal(goal.objective, objective);
    assert.ok(Number.isSafeInteger(goal.tokensUsed) && goal.tokensUsed > 0, "goal must account real model tokens");
    assert.ok(Number.isFinite(goal.timeUsedSeconds) && goal.timeUsedSeconds > 0, "goal must account active elapsed time");
    assert.ok(results.some((row) => row.tool === "exec_command" && row.status === "completed"
      && row.structured_result?.exit_code === 0 && row.structured_result?.output?.includes(marker)), "successful shell verification must return the unique file marker");
    const verified = results.findIndex((row) => row.tool === "exec_command" && row.status === "completed"
      && row.structured_result?.exit_code === 0 && row.structured_result?.output?.includes(marker));
    assert.ok(results.indexOf(completions.at(-1)) > verified, "completion must follow shell verification");
    return goal;
  };

  const created = await command(agent, `/goal ${objective}`);
  assert.ok(created.finalMessage.startsWith("Goal active:"));
  const firstStatus = await waitComplete(agent);
  const first = await resultsSince(agent);
  const completed = goalEvidence(first.results);
  assert.ok(first.results.filter((row) => row.tool === "exec_command" && row.status === "completed"
    && row.structured_result?.exit_code === 0).length >= 2, "initial write and independent read must both execute");
  // Pausing after completion deliberately avoids racing a fast model turn.
  const paused = await command(agent, "/goal pause");
  assert.ok(paused.finalMessage.startsWith("Goal paused:"));
  const reopened = Agent.open(agent.id, options);
  assert.equal(await status(reopened), paused.finalMessage, "paused goal and usage persist across a fresh SDK handle");
  assert.deepEqual((await reopened.state()).active_turns, []);
  assert.equal(await status(reopened), paused.finalMessage, "status reads cannot restart paused goal work");
  const resumed = await command(reopened, "/goal resume");
  assert.ok(resumed.finalMessage.startsWith("Goal active:"));
  const finalStatus = await waitComplete(reopened);
  const second = await resultsSince(reopened, first.cursor);
  const recompleted = goalEvidence(second.results);
  assert.equal(recompleted.goalId, completed.goalId, "resume preserves goal identity");
  assert.ok(recompleted.tokensUsed > completed.tokensUsed, "resumed work accumulates token usage");
  assert.ok(recompleted.timeUsedSeconds >= completed.timeUsedSeconds);
  assert.equal(await status(Agent.open(agent.id, options)), finalStatus, "completed state and final usage persist");
  assert.notEqual(finalStatus, firstStatus, "resumption records additional usage");
  await command(reopened, "/goal clear");
  assert.equal(await status(Agent.open(agent.id, options)), "No goal is set.");
  t.diagnostic("PASS: /goal creation, real get_goal/shell verification/update_goal, positive usage, pause, reconnect, resume, cumulative usage, and clear");
});
