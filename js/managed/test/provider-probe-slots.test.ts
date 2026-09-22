import { test } from 'vitest';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
// Slot claiming deliberately has no runtime I/O; compile the TS dependency graph
// under the same runner used by the routing tests when imported there.
import { claimProbeSlot, PROBE_INTERVAL_MS } from '../src/provider-probe-schedule';
test('duplicate/restarted schedule claims consume a slot only once and retain bounded history', () => {
  const db = new DatabaseSync(':memory:');
  const sql = { exec(query: string, ...bindings: any[]) { const statement=db.prepare(query); return statement.columns().length ? statement.all(...bindings) : (statement.run(...bindings), []); } };
  assert.equal(claimProbeSlot(sql, PROBE_INTERVAL_MS), true);
  assert.equal(claimProbeSlot(sql, PROBE_INTERVAL_MS+100), false);
  assert.equal(claimProbeSlot(sql, NaN), false);
  for (let slot=2;slot<200;slot++) assert.equal(claimProbeSlot(sql,slot*PROBE_INTERVAL_MS),true);
  assert.ok((db.prepare('SELECT COUNT(*) AS n FROM provider_probe_ticks').get() as {n:number}).n<=97);
  db.close();
});
