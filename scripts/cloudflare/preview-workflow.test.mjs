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

function field(source, name) {
  const match = source.match(new RegExp('^    ' + name + ': ([^\\n]*)$', 'm'));
  assert.ok(match, 'missing job field: ' + name);
  if (match[1] !== '>-' && match[1] !== '|') return match[1];
  const remainder = source.slice(match.index + match[0].length + 1);
  return remainder.split('\n').filter((line, index, lines) =>
    line.startsWith('      ') && lines.slice(0, index).every(previous => previous.startsWith('      ')))
    .map(line => line.trim()).join(' ');
}

function block(source, name) {
  const match = source.match(new RegExp('^    ' + name + ':\\n((?:^      .*\\n)*)', 'm'));
  assert.ok(match, 'missing job block: ' + name);
  return match[1];
}

const worker = job('worker-build');
const plan = job('preview-image-plan');
const images = job('preview-images');
const preview = job('preview');
const success = job('preview-success');
const imageSuccess = job('preview-images-success');

test('preview images are explicit opt-in and independent of Worker readiness', () => {
  const eligibility = "vars.CLOUDFLARE_DEPLOY_ENABLED == 'true' && " +
    "((github.event_name == 'pull_request' && " +
    'github.event.pull_request.head.repo.full_name == github.repository) || ' +
    "(github.event_name == 'workflow_dispatch' && inputs.target == 'preview'))";
  assert.equal(field(worker, 'if'), eligibility);
  assert.equal(field(plan, 'if'), "vars.CLOUDFLARE_DEPLOY_ENABLED == 'true' && github.event_name == 'workflow_dispatch' && inputs.target == 'preview' && inputs.validate_images");
  assert.match(workflow, /validate_images:\n        description: [^\n]+\n        type: boolean\n        default: false/);
  assert.equal(field(preview, 'if'), eligibility);
  assert.doesNotMatch(plan, /^    needs:/m);
  assert.equal(field(images, 'needs'), 'preview-image-plan');
  assert.equal(field(images, 'if'), "needs.preview-image-plan.outputs.required == 'true'");
  assert.equal(field(preview, 'needs'), 'worker-build');
});

test('preview image jobs can only plan and build without publishing or credentials', () => {
  const globalEnv = workflow.slice(workflow.indexOf('\nenv:\n'), workflow.indexOf('\njobs:\n'));
  assert.doesNotMatch(globalEnv, /CI_TESTS_ENABLED|CLOUDFLARE_|secrets\.|github\.token/);
  for (const source of [plan, images]) {
    assert.equal(block(source, 'permissions'), '      contents: read\n');
    assert.doesNotMatch(source, /^    environment:|secrets\.|github\.token|CLOUDFLARE_ACCOUNT_ID|CLOUDFLARE_API_TOKEN|GH_TOKEN|GITHUB_TOKEN/m);
    assert.doesNotMatch(source, /^\s+CI_TESTS_ENABLED:/m, 'retain the sandbox Dockerfile default');
    assert.doesNotMatch(source, /WRANGLER_DOCKER_CACHE_WRITE|cache\/save|login-action|--push|--cache-to/);
    assert.match(source, /^          persist-credentials: false$/m);
    assert.doesNotMatch(source, /continue-on-error:/);
  }
});

test('Worker previews retain validation and asset uploads but do not build containers', () => {
  assert.doesNotMatch(preview, /preview-images\.mjs|managed-images\.mjs|setup-buildx-action|WRANGLER_DOCKER_BIN|BUILDX_BUILDER|managed-container/);
  assert.match(preview,
    /      - name: Validate managed Worker\n        working-directory: js\/managed\n        run: npx wrangler deploy --dry-run --config wrangler\.jsonc --containers-rollout none\n/);
});

test('the Worker gate never depends on optional image jobs', () => {
  assert.equal(field(success, 'name'), 'Cloudflare preview success');
  assert.equal(field(success, 'needs'), '[worker-build, preview]');
  assert.equal(field(success, 'if'), 'always() && ' + field(worker, 'if'));
  assert.equal(field(success, 'permissions'), '{}');
  assert.doesNotMatch(success, /continue-on-error:|environment:|secrets\.|needs\.preview-image/);
  assert.equal(field(imageSuccess, 'needs'), '[preview-image-plan, preview-images]');
  assert.equal(field(imageSuccess, 'if'), 'always() && ' + field(plan, 'if'));
});

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
