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
    let lastError;
    while (Date.now() < deadline) {
      attempt += 1;
      let response;
      let text;
      try {
        response = await fetch(`${origin}${path}`, {
          method,
          headers: {
            authorization: `Bearer ${key}`,
            "content-type": "application/json",
            ...(idempotencyKey ? { "idempotency-key": idempotencyKey } : {}),
          },
          ...(encodedBody === undefined ? {} : { body: encodedBody }),
          signal: AbortSignal.timeout(Math.max(1, Math.min(15_000, deadline - Date.now()))),
        });
        text = await response.text();
      } catch (error) {
        lastError = error;
      }
      if (text !== undefined) {
        if (response.ok) {
          if (attempt > 1) t.diagnostic(`${method} ${path} recovered after ${attempt} attempts`);
          // A malformed successful response is a contract failure, not a
          // transport failure that should reissue the request.
          return text ? JSON.parse(text) : undefined;
        }
        lastError = new Error(`${method} ${path}: HTTP ${response.status}: ${text.slice(0, 500)}`);
        const transient = response.status === 408 || response.status === 425
          || response.status === 429 || response.status >= 500;
        if (!transient) throw lastError;
      }
      const retryAfter = response?.headers.get("retry-after");
      const seconds = retryAfter === null || retryAfter === undefined ? Number.NaN : Number(retryAfter);
      const requestedDelay = Number.isFinite(seconds) ? seconds * 1_000
        : retryAfter ? Date.parse(retryAfter) - Date.now() : Number.NaN;
      const backoff = Number.isFinite(requestedDelay)
        ? Math.max(100, requestedDelay) : Math.min(250 * (2 ** (attempt - 1)), 5_000);
      await delay(Math.max(0, Math.min(backoff, deadline - Date.now())));
    }
    throw lastError ?? new Error(`${method} ${path}: request deadline exceeded`);
  };
  const diagnose = async (agentBase, turnId) => {
    const [view, state, history, capacity] = await Promise.all([
      request(`${agentBase}/turns/${turnId}`), request(agentBase),
      request(`${agentBase}/events/history?limit=64`), request(`${agentBase}/capacity`),
    ]);
    // Log state and event identities only. Never log prompts, provider frames,
    // reasoning, tool arguments, or tool output from retained conversations.
    t.diagnostic(JSON.stringify({
      turn: { id: view.turn_id, state: view.state, attempt_count: view.attempt_count,
        retry_at: view.retry_at, updated_at: view.updated_at },
      agent: { loaded: state.agent_loaded, active_turns: state.active_turns,
        stream_failed: Boolean(state.stream_error) },
      durable_state: capacity.durable_state,
      archived_turns: capacity.archived_turns,
      events: history.data.map((event) => ({ cursor: event.cursor,
        turn_id: event.turn_id, created_at: event.created_at,
        type: event.type === "event" ? event.event.type : event.type })),
    }));
  };
  if (process.env.NANOCODEX_DURABILITY_DIAGNOSE_AGENT) {
    try {
      await diagnose(`/v1/agents/${encodeURIComponent(process.env.NANOCODEX_DURABILITY_DIAGNOSE_AGENT)}`,
        process.env.NANOCODEX_DURABILITY_DIAGNOSE_TURN ?? "turn-60");
    } catch (error) { t.diagnostic(`retained-agent diagnostics unavailable: ${error.message}`); }
  }
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
  const terminal = async (turnId, agentBase = base) => {
    const started = Date.now();
    // Responses transports allow five minutes of event silence before retry.
    // Observe that recovery budget rather than declaring a deadlock at two.
    const deadline = started + 7 * 60_000;
    let diagnosed = false;
    while (Date.now() < deadline) {
      const view = await request(`${agentBase}/turns/${turnId}`);
      if (["completed", "failed", "cancelled"].includes(view.state)) {
        if (diagnosed) t.diagnostic(`${turnId} settled as ${view.state} after ${Date.now() - started}ms`);
        return view;
      }
      if (!diagnosed && Date.now() - started >= 120_000) {
        diagnosed = true;
        try { await diagnose(agentBase, turnId); }
        catch (error) { t.diagnostic(`slow-turn diagnostics unavailable: ${error.message}`); }
      }
      await delay(500);
    }
    try { await diagnose(agentBase, turnId); }
    catch (error) { t.diagnostic(`timeout diagnostics unavailable: ${error.message}`); }
    assert.fail(`turn ${turnId} did not settle within seven minutes, including provider idle recovery`);
  };
  try {
    for (const fixture of JSON.parse(process.env.NANOCODEX_DURABILITY_RECOVERY_AGENTS ?? "[]")) {
      const retainedBase = `/v1/agents/${encodeURIComponent(fixture.agent)}`;
      const recovered = await terminal(fixture.turn, retainedBase);
      assert.equal(recovered.state, "completed", `retained ${fixture.agent}/${fixture.turn} must recover`);
      const followOnId = `recovery-${run}`;
      await request(`${retainedBase}/turns`, "POST", {
        id: followOnId, input: "Reply exactly RECOVERED. Use no tools.",
      });
      const continued = await terminal(followOnId, retainedBase);
      assert.equal(continued.state, "completed");
      assert.match(continued.terminal.final_message, /RECOVERED/);
      t.diagnostic(`retained failing thread ${fixture.agent}/${fixture.turn} recovered and continued`);
    }
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
    // Cross the default 512-receipt hot window without hundreds of extra
    // provider calls. Cancellation still exercises durable admission and
    // settlement; old completed results must survive the R2 archive boundary.
    for (let index = 0; index < 432; index += 1) {
      const turnId = `archive-cancel-${index}`;
      await request(`${base}/turns/${turnId}/cancel`, "POST");
      await request(`${base}/turns`, "POST", { id: turnId, input: "Cancelled archive fixture." });
      assert.equal((await terminal(turnId)).state, "cancelled");
    }
    t.diagnostic("528 settled operations; crossed the default 512-receipt hot window");
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
    const capacity = await request(`${base}/capacity`);
    assert.ok(capacity.archived_turns.archived_receipts >= 16,
      "old receipts must actually move into the archive before replay verification");
    assert.ok(capacity.turns.terminal_rows <= 512, "receipt hot storage must stay bounded");
    t.diagnostic(JSON.stringify({ durable_state: capacity.durable_state,
      archived_turns: capacity.archived_turns, hot_terminal_rows: capacity.turns.terminal_rows }));
    t.diagnostic("long thread unloaded; testing cold continuation");
    const archived = completed[0];
    const replay = await request(`${base}/turns`, "POST", {
      id: archived.turnId, input: archived.input,
    }, `${run}-${archived.turnId}`);
    assert.equal(replay.state, "completed");
    const retained = await request(`${base}/turns/${archived.turnId}`);
    assert.deepEqual(retained.terminal, archived.view.terminal);
    assert.equal(retained.terminal_cursor, archived.view.terminal_cursor);
    t.diagnostic("oldest completed receipt replayed exactly after archive and idle unload");
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
    t.diagnostic("96 long turns, 432 archive cancellations, old-turn replays, idle reopen, cancellation, and a tool follow-on passed");
  } finally {
    if (passed) await request(base, "DELETE");
    else t.diagnostic(`retained synthetic failing agent ${id} for diagnosis`);
  }
});
