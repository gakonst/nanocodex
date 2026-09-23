import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const workflow = readFileSync(new URL('../../.github/workflows/js-preview.yml', import.meta.url), 'utf8');
const steps = workflow.split(/^      - /m).slice(1);
function step(pattern) {
  const matches = steps.filter(body => pattern.test(body));
  assert.equal(matches.length, 1, `exactly one step matches ${pattern}`);
  return matches[0];
}
function field(body, key) {
  const value = body.match(new RegExp(`^[ \\t]*${key}: (.+)$`, 'm'))?.[1];
  assert.notEqual(value, undefined, `missing ${key}`);
  return value;
}

// These workflow expressions only use property lookups, string equality and ||.
// Evaluate the actual expressions; absent GitHub properties resolve to ''.
function evaluate(expression, context) {
  return runInNewContext(expression.replace(/^\$\{\{\s*|\s*\}\}$/g, '')
    .replaceAll('inputs.wasm-artifact', 'inputs["wasm-artifact"]'), {
    ...context,
    inputs: { 'wasm-artifact': '', ...context.inputs },
  });
}
function context(event = 'workflow_dispatch', overrides = {}, inputs = {}) {
  return {
    github: {
      workflow: 'CI', event_name: event, repository: 'gakonst/nanocodex',
      ref: 'refs/heads/master', run_id: 100,
      event: { pull_request: { number: event === 'pull_request' ? 17 : '' } },
      ...overrides,
    },
    inputs,
  };
}
const enabled = (body, fixture) => !body.match(/^\s*if:/m) || Boolean(evaluate(field(body, 'if'), fixture));
const download = step(/uses: actions\/download-artifact@/);
const build = step(/run: .*build-js-package\.sh/);
const buildSteps = [step(/uses: dtolnay\/rust-toolchain@/), step(/uses: Swatinem\/rust-cache@/),
  step(/uses: taiki-e\/install-action@/), build];
const install = step(/run: pnpm install --frozen-lockfile/);
const publish = step(/name: Publish immutable commit previews/);
const concurrency = workflow.split('concurrency:\n')[1].split('\njobs:')[0];
const group = fixture => field(concurrency, 'group').replace(/\$\{\{\s*(.*?)\s*\}\}/g,
  (_, expression) => String(evaluate(expression, fixture)));
const cancels = fixture => evaluate(field(concurrency, 'cancel-in-progress'), fixture);

test('callers must supply an artifact while standalone dispatch needs no inputs', () => {
  const triggers = workflow.split('\non:\n')[1].split('\nconcurrency:')[0];
  assert.match(triggers, /workflow_call:\n    inputs:\n      wasm-artifact:/);
  assert.match(triggers, /type: string\n        required: true/);
  assert.match(triggers, /  workflow_dispatch:\s*$/);
});

test('supplied artifacts skip every WASM build prerequisite regardless of the parent event', () => {
  for (const event of ['workflow_dispatch', 'pull_request', 'push', 'schedule']) {
    for (const artifact of ['nanocodex-wasm', 'another-wasm-package', 'false', '0']) {
      const fixture = context(event, {}, { 'wasm-artifact': artifact });
      assert.ok(enabled(download, fixture), `${event}: download ${artifact}`);
      assert.equal(evaluate(field(download.split('        with:\n')[1], 'name'), fixture), artifact);
      for (const body of buildSteps) {
        assert.equal(enabled(body, fixture), false, `${event}: must skip ${body}`);
      }
      assert.ok(enabled(install, fixture));
      assert.ok(enabled(publish, fixture));
    }
  }
});

test('absent or empty artifacts select the complete standalone build path', () => {
  for (const event of ['workflow_dispatch', 'pull_request', 'push', 'schedule']) {
    for (const inputs of [{}, { 'wasm-artifact': '' }]) {
      const fixture = context(event, {}, inputs);
      assert.equal(enabled(download, fixture), false, event);
      for (const body of buildSteps) assert.ok(enabled(body, fixture), `${event}: ${body}`);
      assert.ok(enabled(install, fixture));
      assert.ok(enabled(publish, fixture));
    }
  }
  assert.match(buildSteps[0], /targets: wasm32-unknown-unknown/);
  assert.match(buildSteps[2], /tool: wasm-bindgen-cli@/);
  assert.match(build, /build-js-package\.sh --release/);
});

test('shared outputs come from this run and are installed before either publish path', () => {
  assert.equal(field(download, 'path'), 'js/nanocodex');
  assert.doesNotMatch(download, /^\s*(?:run-id|repository|github-token|continue-on-error):/m);
  assert.ok(steps.indexOf(install) < steps.indexOf(download));
  assert.ok(steps.indexOf(install) < steps.indexOf(build));
  assert.ok(steps.indexOf(download) < steps.indexOf(publish));
  assert.ok(steps.indexOf(build) < steps.indexOf(publish));
  assert.equal(field(publish, 'working-directory'), 'js/nanocodex');
  assert.match(publish, /pkg-pr-new publish --previewVersion --compact\s+--commentWithSha \. \.\.\/nanocodex-vite/);
});

test('publishing retains its repository guard', () => {
  const jobCondition = field(workflow.split('  publish:\n')[1].split('    steps:')[0], 'if');
  assert.equal(evaluate(jobCondition, context()), true);
  assert.equal(evaluate(jobCondition, context('workflow_dispatch', { repository: 'fixture/fork' })), false);
});

test('only newer previews of the same PR and caller share a cancellation group', () => {
  const first = context('pull_request', { ref: 'refs/pull/17/merge' });
  const next = context('pull_request', { ref: 'refs/pull/17/merge', run_id: 101 });
  const otherPR = context('pull_request', { event: { pull_request: { number: 18 } }, ref: 'refs/pull/18/merge' });
  const otherCaller = context('pull_request', { workflow: 'Other CI', ref: 'refs/pull/17/merge' });
  assert.equal(group(first), group(next));
  assert.equal(cancels(first), true);
  assert.equal(cancels(next), true);
  assert.notEqual(group(first), group(otherPR));
  assert.notEqual(group(first), group(otherCaller));
});

test('manual, push and scheduled previews cannot cancel or displace unrelated full runs', () => {
  const fixtures = ['workflow_dispatch', 'push', 'schedule'].flatMap(event => [100, 101, 102].map(run_id =>
    context(event, { run_id })));
  fixtures.push(context('workflow_dispatch', { workflow: 'JavaScript Package Preview', run_id: 103 }));
  const groups = fixtures.map(group);
  assert.equal(new Set(groups).size, groups.length, 'unique groups also prevent pending-run displacement');
  for (const fixture of fixtures) {
    assert.equal(cancels(fixture), false);
    assert.notEqual(group(fixture), group(context('pull_request')));
  }
});

test('preview concurrency has its own namespace, separate from the parent CI', () => {
  const parent = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8')
    .split('concurrency:\n')[1].split('\njobs:')[0];
  for (const event of ['workflow_dispatch', 'pull_request', 'push', 'schedule']) {
    const fixture = context(event);
    const parentGroup = field(parent, 'group').replace(/\$\{\{\s*(.*?)\s*\}\}/g,
      (_, expression) => String(evaluate(expression, fixture)));
    assert.notEqual(group(fixture), parentGroup, event);
  }
});
