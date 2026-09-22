import assert from "node:assert/strict";
import { test } from "node:test";
import { caseMetrics, estimatedCostUsd } from "./evalCoordinator.ts";

test("GPT6 aliases preserve legacy exact-ID prices", () => {
  const usage = { inputTokens: 1000, outputTokens: 1000 };
  for (const [model, expected] of [["sol", .012], ["gpt-6-sol", .012], ["luna", .0006], ["gpt-6-luna", .0006], ["gpt-5.6-sol", .024], ["gpt-5.6-luna", .0014]] as const)
    assert.ok(Math.abs(estimatedCostUsd(model, usage)! - expected) < 1e-12);
});
test("GPT6 long context threshold and fast rates include cache reads and writes", () => {
  for (const [model, scale] of [["sol", 1], ["luna", .05]] as const) {
    const usage = { inputTokens: 1000, cachedInputTokens: 200, cacheWriteTokens: 300, outputTokens: 100, contextTokens: 272000 };
    const base = (500 * 2 + 200 * .2 + 300 * 2.5 + 100 * 10) / 1e6 * scale;
    const long = ((500 * 2 + 200 * .2 + 300 * 2.5) * 2 + 100 * 10 * 1.5) / 1e6 * scale;
    assert.ok(Math.abs(estimatedCostUsd(model, usage)! - base) < 1e-12);
    assert.ok(Math.abs(estimatedCostUsd(model, { ...usage, contextTokens: 272001 })! - long) < 1e-12);
    assert.ok(Math.abs(estimatedCostUsd(model, { ...usage, contextTokens: 272001, fastMode: true })! - long * 2) < 1e-12);
  }
});
test("unknown model or missing usage does not invent a cost", () => {
  assert.equal(estimatedCostUsd("unknown", { inputTokens: 1, outputTokens: 1 }), null);
  assert.equal(estimatedCostUsd("sol", undefined), null);
});

test("case metrics propagate actual cache writes and context without treating aggregate input as context", () => {
  const metrics = caseMetrics({ agent: { fast_mode: true, usage: { input_tokens: 1000, cached_input_tokens: 200, cache_write_input_tokens: 300, context_tokens: 272001, output_tokens: 100 } } });
  assert.equal(metrics.cacheWriteTokens, 300);
  assert.equal(metrics.contextTokens, 272001);
  assert.equal(metrics.fastMode, true);
  assert.equal(estimatedCostUsd("sol", metrics), .01016);
  assert.equal(estimatedCostUsd("sol", { inputTokens: 1000000, outputTokens: 100 }), null);
});
