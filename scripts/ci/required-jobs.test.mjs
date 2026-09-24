import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

// Exercise the actual final gate, including GitHub's bash fail-fast semantics.
const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const gate = workflow.split('      - name: Require every applicable CI job\n')[1]
  .split('        run: |\n')[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');
const always = ['CHANGES'];
const selected = { NATIVE_REQUIRED: ['SHARED_HANDS', 'WINDOWS_HAND', 'VM_GUEST'], VOICE_REQUIRED: ['VOICE_NATIVE'], PYTHON_REQUIRED: ['PYTHON'],
  RUST_REQUIRED: ['QUALITY'], WASM_REQUIRED: ['WASM_BUILD'], BINDINGS_REQUIRED: ['BINDINGS'],
  APPS_REQUIRED: ['APPS'], PREVIEW_REQUIRED: ['PREVIEW'], POLICY_REQUIRED: ['POLICY'], CODEQL_REQUIRED: ['CODEQL'] };
function environment(required) {
  return { TEST: 'skipped', WASM_QUALITY: required ? 'success' : 'skipped', ...Object.fromEntries(always.map(key => [key, 'success'])),
    ...Object.fromEntries(Object.entries(selected).flatMap(([key, jobs]) => [[key, String(required)], ...jobs.map(job => [job, required ? 'success' : 'skipped'])])) };
}
const passes = env => spawnSync('bash', ['-e', '-c', gate], { env: { ...process.env, ...env } }).status === 0;

test('full and intentionally reduced matrices pass the real gate', () => {
  assert.ok(passes(environment(true)));
  assert.ok(passes(environment(false)));
  // CUA-only changes keep the native matrix while skipping voice/Python.
  const cua = { ...environment(false), NATIVE_REQUIRED: 'true',
    SHARED_HANDS: 'success', WINDOWS_HAND: 'success', VM_GUEST: 'success' };
  assert.ok(passes(cua));
  for (const job of selected.NATIVE_REQUIRED) {
    for (const result of ['failure', 'cancelled', 'skipped', '']) {
      assert.equal(passes({ ...cua, [job]: result }), false, `CUA ${job}: ${result}`);
    }
  }
});
test('every required check rejects failure, cancellation, and unexpected skips', () => {
  for (const job of [...always, 'WASM_QUALITY', ...Object.values(selected).flat()]) {
    for (const result of ['failure', 'cancelled', 'skipped', '']) {
      assert.equal(passes({ ...environment(true), [job]: result }), false, `${job}: ${result}`);
    }
  }
});
test('missing selection and unplanned execution fail closed', () => {
  for (const key of Object.keys(selected)) {
    for (const value of ['', 'null', 'invalid']) assert.equal(passes({ ...environment(false), [key]: value }), false);
  }
  for (const job of Object.values(selected).flat()) assert.equal(passes({ ...environment(false), [job]: 'success' }), false);
});

test('paused Rust test job must be explicitly skipped', () => {
  for (const result of ['success', 'failure', 'cancelled', '']) {
    assert.equal(passes({ ...environment(true), TEST: result }), false, `TEST: ${result}`);
  }
});

test('WASM quality is required only for Rust changes with WASM consumers', () => {
  for (const rust of [false, true]) for (const bindings of [false, true]) {
    const env = { ...environment(false), RUST_REQUIRED: String(rust),
      BINDINGS_REQUIRED: String(bindings), QUALITY: rust ? 'success' : 'skipped',
      BINDINGS: bindings ? 'success' : 'skipped', WASM_QUALITY: rust && bindings ? 'success' : 'skipped' };
    assert.ok(passes(env));
    for (const result of ['failure', 'cancelled', '', rust && bindings ? 'skipped' : 'success']) {
      assert.equal(passes({ ...env, WASM_QUALITY: result }), false);
    }
  }
});
