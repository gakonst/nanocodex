import assert from "node:assert/strict";
import test from "node:test";

import { estimatedCostUsd } from "./evalCoordinator.ts";

test("estimates GPT-6 Sol and Luna usage at their published standard rates", () => {
  const usage = { inputTokens: 100_000, cachedInputTokens: 20_000, outputTokens: 100_000 };
  assert.equal(estimatedCostUsd("gpt-6-sol", usage), 1.164);
  assert.equal(estimatedCostUsd("sol", usage), 1.164);
  assert.equal(estimatedCostUsd("gpt-6-luna", usage), 0.0582);
  assert.equal(estimatedCostUsd("luna", usage), 0.0582);
  assert.equal(estimatedCostUsd("gpt-5.6-sol", usage), null);
  assert.equal(estimatedCostUsd("terra", usage), null);
});

test("does not infer a long-context tier from aggregate evaluation usage", () => {
  const aggregate = { inputTokens: 1_000_000, cachedInputTokens: 200_000, outputTokens: 1_000_000 };
  assert.equal(estimatedCostUsd("gpt-6-sol", aggregate), null);
  assert.equal(estimatedCostUsd("gpt-6-luna", aggregate), null);
});
