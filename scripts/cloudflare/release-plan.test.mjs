import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { selectRelease, readPlan, planPath, releaseNeeds, installSelected, buildSelected } from './release-plan.mjs';
import { workerSpecs } from './worker-inputs.mjs';
const fingerprints = Object.fromEntries(Object.keys(workerSpecs).map(name => [name, 'a'.repeat(64)]));

test('selection compares each Worker with its successful fingerprint and force bypasses history', async () => {
  const queried = [];
  const ledger = { async lastSuccessfulFingerprint(name) { queried.push(name); return name === 'x' ? 'b'.repeat(64) : name === 'email' ? null : fingerprints[name]; } };
  const plan = await selectRelease(fingerprints, { ledger, revision: 'revision' });
  assert.deepEqual(plan, { schema: 1, revision: 'revision', fingerprints, selected: ['x', 'email'] });
  assert.deepEqual(queried, Object.keys(workerSpecs));
  assert.deepEqual((await selectRelease(fingerprints, { ledger: { lastSuccessfulFingerprint() { throw Error('must not query'); } }, force: true })).selected, Object.keys(workerSpecs));
  assert.deepEqual((await selectRelease(fingerprints, { ledger: { async lastSuccessfulFingerprint(name) { return fingerprints[name]; } } })).selected, []);
  await assert.rejects(selectRelease({ ...fingerprints, x: 'invalid' }, { ledger }));
  await assert.rejects(selectRelease(fingerprints, { ledger: { async lastSuccessfulFingerprint() { throw Error('unavailable'); } } }), /unavailable/);
});

test('persisted plans reject stale revisions, invalid schemas, duplicate and unknown selections', t => {
  const cwd = mkdtempSync(join(tmpdir(), 'release-plan-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const plan = { schema: 1, revision: 'revision', fingerprints, selected: ['x'] };
  const save = value => writeFileSync(join(cwd, planPath), JSON.stringify(value));
  save(plan); assert.deepEqual(readPlan(cwd, 'revision'), plan);
  assert.throws(() => readPlan(cwd, 'other'));
  for (const patch of [{ schema: 2 }, { selected: ['x', 'x'] }, { selected: ['unknown'] }, { selected: 'x' }, { fingerprints: { x: 'bad' } }]) {
    save({ ...plan, ...patch }); assert.throws(() => readPlan(cwd, 'revision'));
  }
});

const commands = (fn, selected) => { const calls = []; fn({ selected }, (...args) => calls.push(args)); return calls; };
test('empty and API-only selections avoid WASM and unrelated installation/build work', () => {
  assert.deepEqual(releaseNeeds({ selected: [] }), { any: false, wasm: false, workspace: false, astra: false, managed: false, account: false });
  assert.deepEqual(commands(installSelected, []), []); assert.deepEqual(commands(buildSelected, []), []);
  assert.deepEqual(releaseNeeds({ selected: ['x'] }), { any: true, wasm: false, workspace: true, astra: false, managed: false, account: false });
  assert.deepEqual(commands(installSelected, ['x']), [['pnpm', ['install', '--frozen-lockfile', '--filter', 'nanocodex-monorepo', '--filter', '@nanocodex/x-api...'], { stdio: 'inherit' }]]);
  assert.deepEqual(commands(buildSelected, ['x']), [['pnpm', ['exec', 'turbo', 'run', 'build', '--filter', 'nanocodex-tools'], { stdio: 'inherit' }]]);
  assert.deepEqual(commands(buildSelected, ['email']), []);
});

test('WASM consumers deduplicate targets and prepare selected managed and Astra assets', () => {
  const selected = ['egress', 'managed', 'astra'];
  assert.equal(releaseNeeds({ selected }).wasm, true);
  assert.deepEqual(commands(buildSelected, selected), [
    ['pnpm', ['exec', 'turbo', 'run', 'build', '--filter', 'nanocodex', '--filter', 'nanocodex-connect-protocol'], { stdio: 'inherit' }],
    [process.execPath, ['js/managed/scripts/prepare-code-evaluator.mjs'], { stdio: 'inherit' }],
    ['npm', ['run', 'build:client', '--prefix', 'examples/astra-mpp-trial'], { stdio: 'inherit' }],
  ]);
  assert.deepEqual(commands(installSelected, ['astra']), [
    ['pnpm', ['install', '--frozen-lockfile', '--filter', 'nanocodex-monorepo', '--filter', 'nanocodex...', '--filter', 'nanocodex-vite...'], { stdio: 'inherit' }],
    ['npm', ['ci', '--prefix', 'examples/astra-mpp-trial'], { stdio: 'inherit' }],
  ]);
  assert.throws(() => buildSelected({ selected: ['managed'] }, () => { throw Error('build failed'); }), /build failed/);
});
