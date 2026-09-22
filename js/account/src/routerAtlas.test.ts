import { test } from "node:test";
import assert from "node:assert/strict";
import {
  atlasRows,
  globalTotals,
  isSparse,
  latencyScale,
  modelFamily,
  thinkingPoints,
} from "./routerAtlasData.ts";
import type { RouterProvider } from "./routerApi.ts";
const p = (overrides: Partial<RouterProvider> = {}): RouterProvider => ({
  source: "live",
  backend: "cloudflare",
  model: "gpt-5.6-luna",
  effort: "low",
  scope: "deployment_global",
  clientIngressColo: null,
  workerColo: null,
  sampleCount: 10,
  successCount: 8,
  censoredCount: 2,
  generationTtftSampleCount: 2,
  generationTtftP50Ms: 200,
  generationTtftP95Ms: 400,
  fullResponseP50Ms: 900,
  lastObservedAt: 1,
  lastTtftObservedAt: 1,
  httpErrorCount: 0,
  networkErrorCount: 0,
  protocolErrorCount: 0,
  timeoutCount: 2,
  cancelledCount: 0,
  ...overrides,
});
test("global totals exclude overlapping ingress/execution and keep probes separate", () => {
  const rows = [
    p(),
    p({ scope: "client_ingress", clientIngressColo: "LHR" }),
    p({ scope: "worker_colo", workerColo: "SJC" }),
    p({
      source: "probe",
      sampleCount: 3,
      censoredCount: 0,
      generationTtftSampleCount: 3,
    }),
  ];
  assert.deepEqual(globalTotals(rows, "live"), {
    samples: 10,
    failed: 2,
    ttft: 2,
  });
  assert.deepEqual(globalTotals(rows, "probe"), {
    samples: 3,
    failed: 0,
    ttft: 3,
  });
});
test("known aliases align without merging observations, sources, efforts or cohorts", () => {
  const rows = [
    p(),
    p({ model: "openai/gpt-5.6-luna", source: "probe" }),
    p({ effort: "high" }),
    p({ scope: "client_ingress", clientIngressColo: "LHR" }),
  ];
  const grouped = atlasRows(rows);
  assert.equal(grouped.length, 2);
  assert.equal(grouped[0].samples.length, 3);
  assert.deepEqual(
    grouped
      .flatMap((r) => r.samples)
      .sort((a, b) => rows.indexOf(a) - rows.indexOf(b)),
    rows,
  );
  assert.equal(modelFamily("@cf/zai-org/glm-5.3"), "glm-5.3");
  assert.notEqual(
    modelFamily("vendor-a/custom"),
    modelFamily("vendor-b/custom"),
  );
});
test("sparsity uses TTFT count, not attempts; missing remains missing; log axis contains all values", () => {
  assert.equal(isSparse(p()), true);
  assert.equal(isSparse(p({ generationTtftSampleCount: 3 })), false);
  const missing = p({
    generationTtftP50Ms: null,
    generationTtftP95Ms: null,
    generationTtftSampleCount: 0,
  });
  assert.equal(atlasRows([missing])[0].samples[0].generationTtftP50Ms, null);
  const scale = latencyScale([p({ generationTtftP95Ms: 51000 }), missing]);
  assert.equal(scale.x(0), 0);
  assert.equal(scale.x(scale.ceiling), 100);
  assert.ok(scale.x(51000) < 100);
  assert.ok(scale.x(100) < scale.x(200));
});

test("one point per thinking level uses only the selected source, without alias aggregation", () => {
  const live = p({ generationTtftP50Ms: 120 });
  const probe = p({ source: "probe", generationTtftP50Ms: 800 });
  const rows = atlasRows([live, probe]);
  assert.equal(thinkingPoints(rows, "cloudflare", "probe")[0].sample, probe);
  assert.equal(thinkingPoints(rows, "cloudflare", "live")[0].sample, live);
  const split = atlasRows([
    probe,
    p({
      source: "probe",
      model: "openai/gpt-5.6-luna",
      generationTtftP50Ms: 1000,
    }),
  ]);
  assert.equal(thinkingPoints(split, "cloudflare", "probe")[0].sample, null);
  assert.equal(
    thinkingPoints(split, "cloudflare", "probe")[0].matches.length,
    2,
  );
  assert.equal(thinkingPoints(rows, "vercel", "probe")[0].sample, null);
});
