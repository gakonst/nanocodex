import { test } from 'node:test';
import assert from 'node:assert/strict';
import { summarize, markdown } from './timings.mjs';

test('queued timestamps do not masquerade as execution; skipped jobs cost no time', () => {
  const run = { id: 1, name: 'CI', head_sha: '123456789', created_at: '2026-09-18T00:00:00Z', status: 'in_progress' };
  const now = '2026-09-18T00:10:00Z';
  const result = summarize(run, [
    { name: 'queued', status: 'queued', started_at: run.created_at, steps: [] },
    { name: 'skipped', status: 'completed', conclusion: 'skipped', steps: [] },
    { name: 'build', status: 'completed', started_at: '2026-09-18T00:04:00Z', completed_at: '2026-09-18T00:07:00Z', steps: [
      { name: 'compile', started_at: '2026-09-18T00:04:00Z', completed_at: '2026-09-18T00:06:00Z' },
    ] },
  ], now);
  assert.equal(result.elapsed_seconds, 600);
  assert.equal(result.jobs[0].execution_seconds, null);
  assert.equal(result.jobs[0].pre_execution_seconds, 600);
  assert.equal(result.jobs[1].pre_execution_seconds, null);
  assert.equal(result.jobs[2].pre_execution_seconds, 240);
  assert.equal(result.jobs[2].execution_seconds, 180);
  assert.equal(result.jobs[2].steps[0].seconds, 120);
  assert.match(markdown([result]), /compile \(2m00s\)/);
});

test('cancelled queued jobs stop accruing time when they end', () => {
  const run = { created_at: '2026-09-18T00:00:00Z', updated_at: '2026-09-18T00:06:00Z', status: 'completed' };
  const result = summarize(run, [{ name: 'cancelled', status: 'completed', conclusion: 'cancelled', completed_at: '2026-09-18T00:05:00Z', steps: [] }], '2026-09-18T01:00:00Z');
  assert.equal(result.elapsed_seconds, 360);
  assert.equal(result.jobs[0].pre_execution_seconds, 300);
  assert.equal(result.jobs[0].execution_seconds, null);
});
