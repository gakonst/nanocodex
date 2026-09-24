import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const wrapper = fileURLToPath(new URL('./wrangler-docker.mjs', import.meta.url));
function withDockerFixture(check) {
  const dir = mkdtempSync(join(tmpdir(), 'cloudflare-docker-test-'));
  try {
    const capture = join(dir, 'capture.json');
    writeFileSync(join(dir, 'docker'), `#!${process.execPath}
const fs = require('node:fs');
if (process.argv[2] === 'buildx' && process.argv[3] === 'imagetools') {
  fs.appendFileSync(process.env.CAPTURE + '.inspections', JSON.stringify(process.argv.slice(2)) + '\\n');
  process.exit(Number(process.env.INSPECT_EXIT || 0));
}
fs.writeFileSync(process.env.CAPTURE, JSON.stringify({ args: process.argv.slice(2), stdin: fs.readFileSync(0, 'utf8') }));
process.exit(Number(process.env.DOCKER_EXIT || 0));
`, { mode: 0o755 });
    check((args, input = '', overrides = {}) => {
      rmSync(capture, { force: true });
      rmSync(capture + '.inspections', { force: true });
      const result = spawnSync(process.execPath, [wrapper, ...args], {
        input, encoding: 'utf8',
        env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CAPTURE: capture,
          GITHUB_WORKSPACE: '/runner/repo', BUILDX_BUILDER: 'ci-builder',
          GITHUB_REPOSITORY: 'Example/Project', GITHUB_REF: 'refs/pull/7/merge',
          GITHUB_EVENT_NAME: 'pull_request', WRANGLER_DOCKER_CACHE_WRITE: '',
          DOCKER_EXIT: '0', INSPECT_EXIT: '0', ...overrides },
      });
      const inspections = existsSync(capture + '.inspections')
        ? readFileSync(capture + '.inspections', 'utf8').trim().split('\n').map(line => JSON.parse(line)) : [];
      return { result, inspections, ...(existsSync(capture) ? JSON.parse(readFileSync(capture, 'utf8')) : {}) };
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
// Exact argument shape used by installed Wrangler 4.127.1.
const build = ['build', '--load', '-t', 'worker-sandbox:unique-tag', '--platform',
  'linux/amd64', '--provenance=false', '--build-arg', 'VALUE=a b', '-f', '-', '/runner/repo/js/managed'];

test('Wrangler preserves build inputs and isolates registry cache scopes across checkout locations', () => {
  withDockerFixture(invoke => {
    const first = invoke(build, 'FROM scratch\nLABEL version=1\n');
    assert.equal(first.result.status, 0);
    assert.deepEqual(first.args.slice(0, 5), ['buildx', 'build', '--builder', 'ci-builder', '--cache-from']);
    assert.deepEqual(first.args.slice(6), build.slice(1));
    assert.equal(first.stdin, 'FROM scratch\nLABEL version=1\n');
    assert.match(first.args[5], /^type=registry,ref=ghcr\.io\/example\/project-hand:buildcache-cloudflare-v1-[a-f0-9]{16}$/);
    assert.ok(!first.args.includes('--cache-to'));
    const moved = invoke([...build.slice(0, -1), '/other/repo/js/managed'], 'FROM scratch\nLABEL version=2\n', { GITHUB_WORKSPACE: '/other/repo' });
    assert.equal(moved.args[5], first.args[5]);
    assert.equal(moved.stdin, 'FROM scratch\nLABEL version=2\n');
    const phone = invoke([...build.slice(0, -1), '/runner/repo']);
    assert.notEqual(phone.args[5], first.args[5]);
    // Cache metadata is optional outside Actions, including publication fixtures.
    const local = invoke(build, '', { GITHUB_REPOSITORY: '' });
    assert.equal(local.result.status, 0);
    assert.deepEqual(local.args, ['buildx', 'build', '--builder', 'ci-builder', ...build.slice(1)]);
  });
});

test('only opted-in master pushes and dispatches export caches, even with a forged PR master ref', () => {
  withDockerFixture(invoke => {
    for (const event of ['push', 'workflow_dispatch', 'pull_request', 'pull_request_target', 'workflow_run', 'schedule', '']) {
      for (const ref of ['refs/heads/master', 'refs/heads/feature', 'refs/pull/7/merge', 'refs/tags/v1']) {
        const trusted = ref === 'refs/heads/master' && ['push', 'workflow_dispatch'].includes(event);
        const result = invoke(build, 'FROM scratch\n', {
          GITHUB_EVENT_NAME: event, GITHUB_REF: ref, WRANGLER_DOCKER_CACHE_WRITE: 'true',
        });
        assert.equal(result.result.status, 0, result.result.stderr);
        assert.equal(result.args.includes('--cache-to'), trusted, `${event} ${ref}`);
        assert.equal(result.inspections.length, trusted ? 1 : 0, `${event} ${ref}`);
        if (trusted) {
          assert.equal(result.args[7], `${result.args[5]},mode=max,ignore-error=true`);
          assert.deepEqual(result.args.slice(8), build.slice(1));
        }
      }
    }
    for (const enabled of ['', 'false', '1']) {
      const result = invoke(build, '', { GITHUB_REF: 'refs/heads/master', GITHUB_EVENT_NAME: 'push', WRANGLER_DOCKER_CACHE_WRITE: enabled });
      assert.ok(!result.args.includes('--cache-to'));
    }
  });
});

test('Docker passthrough and genuine build failures retain stdin and exit status', () => {
  withDockerFixture(invoke => {
    for (const args of [['image', 'inspect', 'worker:tag'], ['login', '--password-stdin', 'registry.example'], ['tag', 'a', 'b'], ['push', 'b']]) {
      const passthrough = invoke(args, 'test-input', { DOCKER_EXIT: '17', BUILDX_BUILDER: '', GITHUB_REPOSITORY: '' });
      assert.deepEqual(passthrough.args, args);
      assert.equal(passthrough.stdin, 'test-input');
      assert.equal(passthrough.result.status, 17);
    }
    assert.equal(invoke(build, '', { DOCKER_EXIT: '23' }).result.status, 23);
    for (const overrides of [{ BUILDX_BUILDER: '' }, { GITHUB_WORKSPACE: '' }]) {
      const rejected = invoke(build, '', overrides);
      assert.notEqual(rejected.result.status, 0);
      assert.equal(rejected.args, undefined);
      assert.match(rejected.result.stderr, /requires GITHUB_WORKSPACE and BUILDX_BUILDER/);
    }
  });
});

test('cache availability notices are nonfatal and follow successful trusted builds only', () => {
  withDockerFixture(invoke => {
    const trusted = { GITHUB_REF: 'refs/heads/master', GITHUB_EVENT_NAME: 'push', WRANGLER_DOCKER_CACHE_WRITE: 'true' };
    for (const status of ['0', '19']) {
      const result = invoke(build, '', { ...trusted, INSPECT_EXIT: status });
      assert.equal(result.result.status, 0, result.result.stderr);
      assert.deepEqual(result.inspections, [['buildx', 'imagetools', 'inspect', '--raw', result.args[5].split('ref=')[1]]]);
      assert.match(result.result.stdout, status === '0' ? /::notice::Registry cache manifest available:/ : /::notice::Registry cache manifest unavailable:.*Build succeeded/);
    }
    const failed = invoke(build, '', { ...trusted, DOCKER_EXIT: '23' });
    assert.equal(failed.result.status, 23);
    assert.deepEqual(failed.inspections, []);
    assert.doesNotMatch(failed.result.stdout, /::notice::/);
  });
});

const readWorkflow = name => readFileSync(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
const jobs = workflow => Object.fromEntries([...workflow.split('\njobs:\n')[1]
  .matchAll(/^  ([a-z-]+):\n([\s\S]*?)(?=^  [a-z-]+:\n|$(?![\s\S]))/gm)].map(([, name, body]) => [name, body]));
function condition(block) {
  const lines = block.split('\n');
  const index = lines.findIndex(line => /^\s+if: /.test(line));
  assert.ok(index >= 0, 'expected an explicit trust condition');
  const [, indent, value] = lines[index].match(/^(\s+)if: (.+)$/);
  if (value !== '>-') return value;
  const parts = [];
  for (const line of lines.slice(index + 1)) {
    if (!line.startsWith(' '.repeat(indent.length + 1))) break;
    parts.push(line.trim());
  }
  return parts.join(' ');
}
const evaluate = (expression, context) => new Function(...Object.keys(context),
  `return (${expression.replaceAll('steps.cache-login', "steps['cache-login']")});`)(...Object.values(context));

test('Cloudflare PR jobs stay read-only and registry login is limited to trusted master cache writers', () => {
  const workflow = readWorkflow('cloudflare');
  const all = jobs(workflow);
  const publishers = Object.entries(all).filter(([, body]) => /packages: write/.test(body)).map(([name]) => name);
  assert.deepEqual(publishers, ['managed-images']);
  const login = all['managed-images'].split('      - ').find(step => step.includes('id: cache-login'));
  assert.match(login, /continue-on-error: true/);
  assert.match(all['managed-images'], /WRANGLER_DOCKER_CACHE_WRITE: \$\{\{ steps\.cache-login\.outcome == 'success' \}\}/);
  for (const event_name of ['push', 'workflow_dispatch', 'pull_request', 'pull_request_target', 'workflow_run', 'schedule']) {
    for (const ref of ['refs/heads/master', 'refs/heads/feature', 'refs/pull/7/merge']) {
      const github = { event_name, ref };
      const trusted = ref === 'refs/heads/master' && ['push', 'workflow_dispatch'].includes(event_name);
      for (const hit of ['true', 'false']) {
        assert.equal(evaluate(condition(login), { github, steps: { available: { outputs: { hit } } } }), trusted && hit !== 'true');
      }
      if (event_name.startsWith('pull_request')) {
        assert.equal(evaluate(condition(all['image-plan']), { github, vars: { CLOUDFLARE_DEPLOY_ENABLED: 'true' }, inputs: { target: 'production' } }), false);
      }
    }
  }
  for (const name of ['worker-build', 'preview']) {
    assert.doesNotMatch(all[name], /packages: write|docker\/login-action|WRANGLER_DOCKER_CACHE_WRITE/);
  }
});

test('toolkit PRs import anonymously; only master dispatches get a package writer and optional exports', () => {
  const workflow = readWorkflow('hand-toolkit');
  const all = jobs(workflow);
  assert.doesNotMatch(all.image, /packages: write|docker\/login-action|cache-to:|imagetools inspect/);
  assert.match(all['image-cache'], /packages: write/);
  const login = all['image-cache'].split('      - ').find(step => step.includes('id: cache-login'));
  assert.match(login, /continue-on-error: true/);
  for (const event_name of ['workflow_dispatch', 'pull_request', 'pull_request_target', 'push', 'workflow_run']) {
    for (const ref of ['refs/heads/master', 'refs/heads/feature', 'refs/pull/7/merge']) {
      const github = { event_name, ref };
      const trusted = event_name === 'workflow_dispatch' && ref === 'refs/heads/master';
      assert.equal(evaluate(condition(all['image-cache']), { github }), trusted);
      assert.equal(evaluate(condition(all.image), { github }), !trusted);
    }
  }
  const availability = all['image-cache'].split('      - ').find(step => step.includes('name: Report toolkit cache availability'));
  for (const outcome of ['success', 'failure', 'skipped', 'cancelled']) {
    assert.equal(evaluate(condition(availability), { steps: { 'cache-login': { outcome } } }), outcome === 'success');
  }
  const exportExpression = all['image-cache'].match(/cache-to: \$\{\{ (.+) \}\}/)[1];
  for (const outcome of ['success', 'failure', 'skipped', 'cancelled']) {
    const ref = 'ghcr.io/example/project-hand:buildcache-vm-toolkit-ubuntu-24.04-Dockerfile';
    const value = evaluate(exportExpression, { steps: { 'cache-login': { outcome }, cache: { outputs: { ref } } },
      format: (template, value) => template.replace('{0}', value) });
    assert.equal(value, outcome === 'success' ? `type=registry,ref=${ref},mode=max,ignore-error=true` : '');
  }
});
