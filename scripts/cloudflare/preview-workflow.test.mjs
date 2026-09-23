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

function runs(source) {
  return [...source.matchAll(/^        run: (.*)$/gm)].map(match => match[1]);
}

function uses(source) {
  return [...source.matchAll(/^      - uses: ([^@\n]+)@/gm)].map(match => match[1]);
}

const worker = job('worker-build');
const plan = job('preview-image-plan');
const images = job('preview-images');
const preview = job('preview');
const success = job('preview-success');

test('preview planning and image builds run independently of Worker builds', () => {
  const eligibility = "vars.CLOUDFLARE_DEPLOY_ENABLED == 'true' && " +
    "((github.event_name == 'pull_request' && " +
    'github.event.pull_request.head.repo.full_name == github.repository) || ' +
    "(github.event_name == 'workflow_dispatch' && inputs.target == 'preview'))";
  assert.equal(field(worker, 'if'), eligibility);
  assert.equal(field(plan, 'if'), eligibility);
  assert.equal(field(preview, 'if'), eligibility);
  assert.doesNotMatch(plan, /^    needs:/m);
  assert.equal(field(images, 'needs'), 'preview-image-plan');
  assert.equal(field(images, 'if'), "needs.preview-image-plan.outputs.required == 'true'");
  assert.equal(field(preview, 'needs'), 'worker-build');
  assert.equal(block(images, 'strategy'),
    '      fail-fast: false\n' +
    '      matrix: ${{ fromJSON(needs.preview-image-plan.outputs.matrix) }}\n');
});

test('the plan exposes required and matrix outputs without credentials', () => {
  assert.equal(block(plan, 'outputs'),
    '      required: ${{ steps.plan.outputs.required }}\n' +
    '      matrix: ${{ steps.plan.outputs.matrix }}\n');
  assert.deepEqual(uses(plan), ['actions/checkout', 'actions/setup-node']);
  assert.match(plan, /^          fetch-depth: 0$/m);
  assert.match(plan, /^          node-version: 24$/m);
  assert.match(plan, /^        id: plan$/m);
  assert.match(plan, /^          BASE_SHA: \$\{\{ github\.event\.pull_request\.base\.sha \|\| github\.event\.before \}\}$/m);
  assert.deepEqual(runs(plan), ['node scripts/cloudflare/preview-images.mjs']);
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
    assert.match(source, /^          node-version: 24$/m);
    assert.doesNotMatch(source, /continue-on-error:/);
  }
  assert.deepEqual(uses(images), ['actions/checkout', 'actions/setup-node', 'docker/setup-buildx-action']);
  assert.deepEqual(runs(images), ['node scripts/cloudflare/managed-images.mjs preview "$IMAGE"']);
  assert.match(images, /^        id: builder$/m);
  assert.match(images, /^          IMAGE: \$\{\{ matrix\.image \}\}$/m);
  assert.match(images, /^          BUILDX_BUILDER: \$\{\{ steps\.builder\.outputs\.name \}\}$/m);
});

test('Worker previews retain validation and asset uploads but do not build containers', () => {
  assert.doesNotMatch(preview, /preview-images\.mjs|managed-images\.mjs|setup-buildx-action|WRANGLER_DOCKER_BIN|BUILDX_BUILDER|managed-container/);
  assert.match(preview,
    /      - name: Validate managed Worker\n        working-directory: js\/managed\n        run: npx wrangler deploy --dry-run --config wrangler\.jsonc --containers-rollout none\n/);
  for (const name of [
    'Restore same-revision Worker outputs',
    'Validate egress Worker',
    'Validate email Worker',
    'Validate X Worker',
    'Validate Connect API Worker',
    'Validate Astra trial Worker',
    'Validate Chief of Staff Worker',
    'Validate account Worker',
    'Upload Connect dialog preview version',
    'Upload Connect playground preview version',
  ]) assert.ok(preview.includes('      - name: ' + name + '\n'), 'missing preview step: ' + name);
  assert.equal((preview.match(/run: npx wrangler versions upload /g) ?? []).length, 2);
  for (const source of [worker, preview]) {
    assert.match(source,
      /      - name: Test Wrangler image cache boundary\n        # Temporarily disabled; re-enable when CI test coverage resumes\.\n        if: false\n/);
  }
});

test('the final check runs for eligible previews even when dependencies fail or skip', () => {
  assert.equal(field(success, 'name'), 'Cloudflare preview success');
  assert.equal(field(success, 'needs'), '[worker-build, preview-image-plan, preview-images, preview]');
  assert.equal(field(success, 'if'), 'always() && ' + field(worker, 'if'));
  assert.equal(field(success, 'permissions'), '{}');
  assert.doesNotMatch(success, /continue-on-error:|environment:|secrets\./);
  assert.match(success, /^        shell: bash$/m);
  for (const [variable, expression] of Object.entries({
    WORKER_BUILD_RESULT: 'needs.worker-build.result',
    IMAGE_PLAN_RESULT: 'needs.preview-image-plan.result',
    PREVIEW_IMAGES_RESULT: 'needs.preview-images.result',
    PREVIEW_RESULT: 'needs.preview.result',
    IMAGES_REQUIRED: 'needs.preview-image-plan.outputs.required',
  })) {
    assert.ok(success.includes('          ' + variable + ': ${{ ' + expression + ' }}\n'),
      'missing gate input: ' + variable);
  }
});

const gateMatch = success.match(/^        run: \|\n((?:^          .*\n|^\n)+)/m);
assert.ok(gateMatch, 'missing executable preview gate');
const gate = gateMatch[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');

function runGate(env) {
  const result = spawnSync('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', gate], {
    env, encoding: 'utf8', timeout: 5_000,
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, result.stderr);
  return result;
}

test('the actual Bash gate accepts only complete successful previews', () => {
  const complete = {
    WORKER_BUILD_RESULT: 'success',
    IMAGE_PLAN_RESULT: 'success',
    PREVIEW_IMAGES_RESULT: 'success',
    PREVIEW_RESULT: 'success',
    IMAGES_REQUIRED: 'true',
  };
  const check = (env, expected) => {
    const result = runGate(env);
    assert.equal(result.status === 0, expected,
      JSON.stringify(env) + '\n' + result.stdout + result.stderr);
  };
  // Cover each prerequisite independently for both valid image plans. Crossing
  // every failing prerequisite creates thousands of redundant Bash processes.
  for (const [required, imageResult] of [['true', 'success'], ['false', 'skipped']]) {
    const valid = { ...complete, IMAGES_REQUIRED: required, PREVIEW_IMAGES_RESULT: imageResult };
    check(valid, true);
    for (const key of ['WORKER_BUILD_RESULT', 'IMAGE_PLAN_RESULT', 'PREVIEW_RESULT']) {
      for (const result of ['failure', 'cancelled', 'skipped', '']) {
        check({ ...valid, [key]: result }, false);
      }
    }
  }
  for (const required of ['true', 'false', '', 'unexpected']) {
    for (const imageResult of ['success', 'failure', 'cancelled', 'skipped', '']) {
      check({ ...complete, IMAGES_REQUIRED: required, PREVIEW_IMAGES_RESULT: imageResult },
        (required === 'true' && imageResult === 'success') ||
        (required === 'false' && imageResult === 'skipped'));
    }
  }
});

test('the actual Bash gate fails if any required environment input is missing', () => {
  const complete = {
    WORKER_BUILD_RESULT: 'success',
    IMAGE_PLAN_RESULT: 'success',
    PREVIEW_IMAGES_RESULT: 'success',
    PREVIEW_RESULT: 'success',
    IMAGES_REQUIRED: 'true',
  };
  for (const key of Object.keys(complete)) {
    const env = { ...complete };
    delete env[key];
    assert.notEqual(runGate(env).status, 0, 'missing gate input must fail: ' + key);
  }
});
