import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";

// Explicitly invoked by the production deployment job, never by the unit suite.
// Exercise the public API with synthetic conversations under the CI account.
test("deployed durable threads survive long histories, replay, tools, and cancellation", {
  timeout: 18 * 60_000,
}, async (t) => {
  const key = process.env.NANOCODEX_DURABILITY_TEST_API_KEY;
  assert.ok(key, "NANOCODEX_ASTRA_MANAGED_API_KEY is required for deployed durability evidence");
  const origin = "https://nanocodex.gakonst.workers.dev";
  const run = `durability-${process.env.GITHUB_RUN_ID ?? "local"}-${randomUUID()}`;
  const request = async (path, method = "GET", body, idempotencyKey) => {
    const deadline = Date.now() + 45_000;
    const encodedBody = body === undefined ? undefined : JSON.stringify(body);
    let attempt = 0;
    for (;;) {
      attempt += 1;
      try {
        const response = await fetch(`${origin}${path}`, {
          method,
          headers: {
            authorization: `Bearer ${key}`,
            "content-type": "application/json",
            ...(idempotencyKey ? { "idempotency-key": idempotencyKey } : {}),
          },
          ...(encodedBody === undefined ? {} : { body: encodedBody }),
          signal: AbortSignal.timeout(15_000),
        });
        const text = await response.text();
        if (response.ok) {
          if (attempt > 1) t.diagnostic(`${method} ${path} recovered after ${attempt} attempts`);
          return text ? JSON.parse(text) : undefined;
        }
        const transient = response.status === 408 || response.status === 425
          || response.status === 429 || response.status >= 500;
        if (!transient || Date.now() >= deadline) {
          assert.fail(`${method} ${path}: HTTP ${response.status}: ${text.slice(0, 500)}`);
        }
        const retryAfterHeader = response.headers.get("retry-after");
        const retryAfter = retryAfterHeader === null ? Number.NaN : Number(retryAfterHeader);
        const backoff = Number.isFinite(retryAfter) && retryAfter >= 0
          ? retryAfter * 1_000
          : Math.min(250 * (2 ** (attempt - 1)), 5_000);
        await delay(Math.min(backoff, Math.max(0, deadline - Date.now())));
      } catch (error) {
        if (error?.code === "ERR_ASSERTION" || Date.now() >= deadline) throw error;
        await delay(Math.min(250 * (2 ** (attempt - 1)), 5_000, deadline - Date.now()));
      }
    }
  };
  const created = await request("/v1/agents", "POST", {
    settings: {
      model: "gpt-5.6-luna", thinking: "low", reasoning_mode: "standard", fast_mode: false,
    },
  }, run);
  const id = created.id ?? created.agent_id;
  assert.equal(typeof id, "string");
  const base = `/v1/agents/${encodeURIComponent(id)}`;
  t.diagnostic(`synthetic agent ${id}; deployment ${process.env.GITHUB_SHA ?? "local"}`);
  let passed = false;
  const terminal = async (turnId) => {
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      const view = await request(`${base}/turns/${turnId}`);
      if (["completed", "failed", "cancelled"].includes(view.state)) return view;
      await delay(500);
    }
    assert.fail(`turn ${turnId} did not settle within two minutes`);
  };
  try {
    const completed = [];
    const context = Array.from({ length: 160 }, (_, index) =>
      `Synthetic record ${index}: durability preserves operation order, exact inputs, and committed results.`).join("\n");
    for (let index = 0; index < 96; index += 1) {
      const turnId = `turn-${index}`;
      const marker = `CHECK_${index}_${run.slice(-8)}`;
      const input = `This is a durability test. Read the synthetic records below and reply with exactly ${marker}. Use no tools for this turn.\n${context}\nReply only ${marker}.`;
      await request(`${base}/turns`, "POST", { id: turnId, input }, `${run}-${turnId}`);
      const view = await terminal(turnId);
      assert.equal(view.state, "completed", JSON.stringify(view));
      assert.ok(view.terminal.final_message.includes(marker), `turn ${index} lost its current input`);
      completed.push({ turnId, input, view });
      if (index % 12 === 11) {
        const old = completed[0];
        const replay = await request(`${base}/turns`, "POST", {
          id: old.turnId, input: old.input,
        }, `${run}-${old.turnId}`);
        assert.equal(replay.state, "completed");
        const retained = await request(`${base}/turns/${old.turnId}`);
        assert.deepEqual(retained.terminal, old.view.terminal);
        assert.equal(retained.terminal_cursor, old.view.terminal_cursor);
        t.diagnostic(`${index + 1} ordered turns; exact replay verified`);
      }
    }
    // Status reads do not keep the runtime warm. Observe the configured idle
    // shutdown before the next turn, rather than guessing with a fixed sleep.
    const idleDeadline = Date.now() + 180_000;
    let unloaded = false;
    while (Date.now() < idleDeadline) {
      const state = await request(base);
      assert.deepEqual(state.active_turns, []);
      if (!state.agent_loaded) { unloaded = true; break; }
      await delay(5_000);
    }
    assert.ok(unloaded, "the long thread must unload before testing cold continuation");
    t.diagnostic("long thread unloaded; testing cold continuation");
    // A deterministic pre-admission cancellation cannot race a fast model
    // completion. The next prompt must progress without manual recovery.
    await request(`${base}/turns/cancelled/cancel`, "POST");
    await request(`${base}/turns`, "POST", { id: "cancelled", input: "This turn is cancelled before admission." });
    assert.equal((await terminal("cancelled")).state, "cancelled");
    await request(`${base}/turns`, "POST", {
      id: "tool-follow-on",
      input: "Use Code Mode to calculate 127 * 131 in JavaScript and print the result. No external tools, files, or sandboxes are needed. Then answer with the number.",
    });
    const followOn = await terminal("tool-follow-on");
    assert.equal(followOn.state, "completed", JSON.stringify(followOn));
    assert.match(followOn.terminal.final_message, /16637/);
    let before;
    let sawToolResult = false;
    for (;;) {
      const page = await request(`${base}/events/history?limit=128${before ? `&before=${before}` : ""}`);
      sawToolResult ||= page.data.some((event) => event.turn_id === "tool-follow-on"
        && event.type === "event" && event.event.type === "tool.result");
      const oldest = page.data.reduce((cursor, event) =>
        cursor === undefined || BigInt(event.cursor) < BigInt(cursor) ? event.cursor : cursor, undefined);
      if (sawToolResult || !page.has_more || oldest === undefined
        || BigInt(oldest) <= BigInt(followOn.accepted_cursor)) break;
      before = oldest;
    }
    assert.ok(sawToolResult, "cold follow-on must execute a tool, not just return mental arithmetic");
    const state = await request(base);
    assert.deepEqual(state.active_turns, []);
    passed = true;
    t.diagnostic("96 long turns, eight old-turn replays, idle reopen, cancellation, and a tool follow-on passed");
  } finally {
    if (passed) await request(base, "DELETE");
    else t.diagnostic(`retained synthetic failing agent ${id} for diagnosis`);
  }
});
