import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/cloudflare.yml', import.meta.url), 'utf8');

// Extract the small subset needed here without installing a YAML parser. The
// executable gate is read from the workflow itself, so its behavior cannot drift
// from a separate test implementation.
function job(name) {
  const jobs = [...workflow.matchAll(/^  ([a-z][a-z0-9-]*):\n/gm)];
  const index = jobs.findIndex(match => match[1] === name);
  assert.notEqual(index, -1, 'missing job: ' + name);
  return workflow.slice(jobs[index].index, jobs[index + 1]?.index ?? workflow.length);
}

const success = job('preview-success');
const imageSuccess = job('preview-images-success');

function runGate(source, env) {
  const match = source.match(/^        run: \|\n((?:^          .*\n|^\n)+)/m);
  assert.ok(match, 'missing executable gate');
  const gate = match[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');
  const result = spawnSync('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', gate], {
    env, encoding: 'utf8', timeout: 5_000,
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, result.stderr);
  return result;
}

test('the actual Worker gate requires successful builds/uploads but ignores image outcomes', () => {
  const complete = { WORKER_BUILD_RESULT: 'success', PREVIEW_RESULT: 'success' };
  for (const image of ['success', 'failure', 'cancelled', 'skipped', '', 'in_progress']) {
    assert.equal(runGate(success, { ...complete, PREVIEW_IMAGES_RESULT: image }).status, 0);
  }
  for (const key of Object.keys(complete)) {
    for (const value of ['failure', 'cancelled', 'skipped', '']) {
      assert.notEqual(runGate(success, { ...complete, [key]: value }).status, 0);
    }
    const missing = { ...complete }; delete missing[key];
    assert.notEqual(runGate(success, missing).status, 0);
  }
});

test('explicit image validation retains a separate strict failure signal', () => {
  const complete = { IMAGE_PLAN_RESULT: 'success', PREVIEW_IMAGES_RESULT: 'success', IMAGES_REQUIRED: 'true' };
  for (const required of ['true', 'false', '', 'unexpected']) {
    for (const result of ['success', 'failure', 'cancelled', 'skipped', '']) {
      const expected = (required === 'true' && result === 'success') || (required === 'false' && result === 'skipped');
      assert.equal(runGate(imageSuccess, { ...complete, IMAGES_REQUIRED: required, PREVIEW_IMAGES_RESULT: result }).status === 0, expected);
    }
  }
  for (const result of ['failure', 'cancelled', 'skipped', '']) {
    assert.notEqual(runGate(imageSuccess, { ...complete, IMAGE_PLAN_RESULT: result }).status, 0);
  }
  for (const key of Object.keys(complete)) {
    const missing = { ...complete }; delete missing[key];
    assert.notEqual(runGate(imageSuccess, missing).status, 0);
  }
});
