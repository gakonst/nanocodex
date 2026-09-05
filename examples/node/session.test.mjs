import assert from "node:assert/strict";
import { test } from "node:test";

import { runOwnedSession } from "./session.mjs";

test("the Node example reads typed results and releases every handle", async () => {
  const harness = createHarness(["42", "43"]);
  const logs = [];
  const results = await runOwnedSession(harness.agent, {
    log: (...values) => logs.push(values),
    logDiagnostic: (value) => logs.push([value]),
  });

  assert.equal(results.first, "42");
  assert.equal(results.second, "43");
  assert.deepEqual(logs, [
    ["tool: multiply"],
    ["first:", "42"],
    ["second:", "43"],
  ]);
  assert.deepEqual(
    harness.prompts,
    [
      "Use multiply to calculate 6 × 7. Return only the number.",
      "Add one to that result. Return only the number.",
    ],
  );
  assert.deepEqual(harness.disposedTurns, [1, 1]);
  assert.deepEqual(harness.disposedResults, [1, 1]);
  assert.equal(harness.unwatched, 1);
  assert.equal(harness.watchOffs, 1);
  assert.equal(harness.agentShutdowns, 1);
  assert.equal(harness.agentDisposals, 0);
  assert.deepEqual(harness.cleanup, [
    "result:1",
    "result:2",
    "turn:1",
    "turn:2",
    "unwatch",
    "watch.off",
    "shutdown",
  ]);
});

test("a rejected result still releases the accepted Turn and agent", async () => {
  const failure = new Error("model failed");
  const harness = createHarness([failure]);

  await assert.rejects(
    runOwnedSession(harness.agent, {
      log() {},
      logDiagnostic() {},
    }),
    failure,
  );
  assert.deepEqual(harness.disposedTurns, [1]);
  assert.deepEqual(harness.disposedResults, [0]);
  assert.equal(harness.unwatched, 1);
  assert.equal(harness.watchOffs, 1);
  assert.equal(harness.agentShutdowns, 1);
  assert.equal(harness.agentDisposals, 0);
  assert.deepEqual(harness.cleanup, ["turn:1", "unwatch", "watch.off", "shutdown"]);
});

function createHarness(outputs) {
  const prompts = [];
  const disposedTurns = [];
  const disposedResults = [];
  const cleanup = [];
  let unwatched = 0;
  let watchOffs = 0;
  let agentDisposals = 0;
  let agentShutdowns = 0;
  const agent = {
    events: {
      watch() {
        return {
          onEvent(listener) {
            listener({
              type: "tool.call",
              payload: { tool: "multiply" },
            });
            return () => {
              unwatched += 1;
              cleanup.push("unwatch");
            };
          },
          off() {
            watchOffs += 1;
            cleanup.push("watch.off");
          },
        };
      },
    },
    session: {
      async shutdown() {
        agentShutdowns += 1;
        cleanup.push("shutdown");
      },
    },
    turn: {
      prompt({ input }) {
        prompts.push(input);
        const output = outputs[prompts.length - 1];
        const index = disposedTurns.push(0) - 1;
        disposedResults.push(0);
        return {
          async result() {
            if (output instanceof Error) throw output;
            return turnResult(output, () => {
              disposedResults[index] += 1;
              cleanup.push(`result:${index + 1}`);
            });
          },
          dispose() {
            disposedTurns[index] += 1;
            cleanup.push(`turn:${index + 1}`);
          },
        };
      },
    },
    dispose() {
      agentDisposals += 1;
      cleanup.push("dispose");
    },
  };
  return {
    agent,
    cleanup,
    disposedResults,
    disposedTurns,
    prompts,
    get unwatched() {
      return unwatched;
    },
    get watchOffs() {
      return watchOffs;
    },
    get agentDisposals() {
      return agentDisposals;
    },
    get agentShutdowns() {
      return agentShutdowns;
    },
  };
}

function turnResult(finalMessage, dispose) {
  return {
    finalMessage,
    dispose,
    snapshot: {
      version: 1,
      model: "gpt-5.6-sol",
      lineage_id: "lineage",
      prompt_cache_key: "cache",
      workspace: "/workspace",
      canonical_context: {},
      history: [],
    },
    usage: {
      input_tokens: 1,
      cached_input_tokens: 0,
      cache_write_input_tokens: 0,
      output_tokens: 1,
      reasoning_output_tokens: 0,
      total_tokens: 2,
      estimated_cost: null,
      cost_status: "usage_not_reported",
    },
  };
}
