import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

for (const benchmark of ['agents-comparison', 'cloudflare-agents']) {
  test(`${benchmark} analyzes GPT-6 and historical model usage without relabeling`, () => {
    const directory = mkdtempSync(join(tmpdir(), 'nanocodex-model-pricing-'));
    try {
      const costs = { 'gpt-6-sol': 0.000264, 'gpt-6-luna': 0.0000132, 'gpt-5.6-luna': 0.0000284 };
      const records = Object.keys(costs).flatMap(model => ['default', 'fast'].map(tier => ({
        model, tier, path: 'responses_http', effort: 'low', workload: 'extraction', state: 'fresh',
        correct: true, terminal: benchmark === 'agents-comparison' ? 'response.completed' : 'completed',
        events: [], ttft_ms: 500, completion_ms: 1000,
        usage: { input_tokens: 100, output_tokens: 10, input_tokens_details: { cached_tokens: 20, cache_write_tokens: 0 } },
      })));
      const input = join(directory, 'matrix.json');
      writeFileSync(input, JSON.stringify({ label: 'matrix', models: Object.keys(costs), tiers: ['default', 'fast'], records }));
      const result = spawnSync(process.execPath, [fileURLToPath(new URL(`../scripts/${benchmark}.analyze.mjs`, import.meta.url)), input], { encoding: 'utf8' });
      assert.equal(result.status, 0, result.stderr);
      const analyzed = JSON.parse(readFileSync(join(directory, 'measurements.json'), 'utf8')).records;
      assert.deepEqual(analyzed.map(r => r.model), records.map(r => r.model));
      for (const row of analyzed) {
        const cost = row.estimated_model_cost ?? row.cost;
        const expected = costs[row.model] * (row.tier === 'fast' ? 2 : 1);
        assert.ok(Math.abs(cost.low - expected) < 1e-12, JSON.stringify(row));
        assert.equal(cost.high, cost.low);
      }
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
}
