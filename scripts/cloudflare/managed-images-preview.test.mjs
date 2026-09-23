import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const helper = fileURLToPath(new URL('./managed-images.mjs', import.meta.url));
const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'managed-images-preview-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const log = join(cwd, 'calls.jsonl');
  const boundary = join(cwd, 'boundary.mjs');
  const write = (path, source, mode) => {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, source, { mode });
  };
  write(boundary, `
import { appendFileSync } from 'node:fs';
export function record(kind, args) {
  appendFileSync(process.env.FIXTURE_LOG, JSON.stringify({
    kind, args,
    cacheWrite: process.env.WRANGLER_DOCKER_CACHE_WRITE,
    testsEnabled: process.env.CI_TESTS_ENABLED ?? null,
    account: process.env.CLOUDFLARE_ACCOUNT_ID ?? null,
  }) + '\\n');
  if (process.env.FIXTURE_FAIL_KIND === kind) {
    console.error('fixture failure: ' + kind);
    process.exit(23);
  }
  if (['docker', 'pnpm', 'git', 'python3'].includes(kind)) {
    console.error('forbidden preview command: ' + kind);
    process.exit(99);
  }
}
`);
  const boundaryImport = JSON.stringify(pathToFileURL(boundary).href);
  write(join(cwd, 'scripts/cloudflare/wrangler-docker.mjs'),
    `import { record } from ${boundaryImport}; record('build', process.argv.slice(2));\n`);
  write(join(cwd, 'js/managed/scripts/prepare-hand-image.mjs'),
    `import { record } from ${boundaryImport}; record('prepare', process.argv.slice(2));\n`);
  const driver = join(cwd, 'command.mjs');
  write(driver, `import { record } from ${boundaryImport}; record(process.argv[2], process.argv.slice(3));\n`);
  for (const command of ['docker', 'pnpm', 'git', 'python3']) {
    // Run the actual Node executable even when the fixture controls PATH.
    write(join(cwd, 'bin', command),
      `#!/bin/sh\nexec ${shellQuote(process.execPath)} ${shellQuote(driver)} ${shellQuote(command)} "$@"\n`, 0o755);
  }
  return {
    run(image, overrides = {}) {
      const env = {
        ...process.env,
        PATH: `${join(cwd, 'bin')}${process.platform === 'win32' ? ';' : ':'}${process.env.PATH ?? ''}`,
        FIXTURE_LOG: log,
        MANAGED_IMAGE_CACHE_EPOCH: 'preview-test-epoch',
        GITHUB_REF: 'refs/heads/master',
        GITHUB_EVENT_NAME: 'push',
        WRANGLER_DOCKER_CACHE_WRITE: 'true',
        ...overrides,
      };
      delete env.CLOUDFLARE_ACCOUNT_ID;
      delete env.CLOUDFLARE_API_TOKEN;
      if (!Object.hasOwn(overrides, 'CI_TESTS_ENABLED')) delete env.CI_TESTS_ENABLED;
      if (!Object.hasOwn(overrides, 'FIXTURE_FAIL_KIND')) delete env.FIXTURE_FAIL_KIND;
      const result = spawnSync(process.execPath, [helper, 'preview', image], {
        cwd, env, encoding: 'utf8', timeout: 15_000,
      });
      assert.ifError(result.error);
      assert.equal(result.signal, null, result.stderr);
      const calls = existsSync(log)
        ? readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line))
        : [];
      assert.equal(existsSync(join(cwd, '.ci-images')), false, 'preview must not create receipts');
      assert.equal(calls.some(call => ['docker', 'pnpm', 'git', 'python3'].includes(call.kind)), false,
        'preview must not run post-build checks, publish, or fingerprint');
      return { ...result, calls };
    },
  };
}

function expectedBuild(image, testsEnabled = 'true') {
  return {
    kind: 'build',
    args: ['build', '--output', 'type=cacheonly', '-t', `nanocodex-ci-${image}:preview`,
      '--platform', 'linux/amd64', '--provenance=false', '--pull',
      '--build-arg', 'NANOCODEX_IMAGE_CACHE_EPOCH=preview-test-epoch',
      ...(image === 'sandbox' ? ['--build-arg', `CI_TESTS_ENABLED=${testsEnabled}`] : []),
      '-f', image === 'sandbox' ? 'js/managed/Dockerfile' : 'js/phone-cloud/Dockerfile',
      image === 'sandbox' ? 'js/managed' : '.'],
  };
}

const commands = calls => calls.map(({ kind, args }) => ({ kind, args }));

for (const image of ['phone', 'sandbox']) {
  for (const testsEnabled of ['false', 'true', undefined]) {
    test(`${image} preview with CI_TESTS_ENABLED=${testsEnabled ?? '(unset)'}`, t => {
      const overrides = testsEnabled === undefined ? {} : { CI_TESTS_ENABLED: testsEnabled };
      const result = fixture(t).run(image, overrides);
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(commands(result.calls), [
        ...(image === 'sandbox' ? [{ kind: 'prepare', args: [] }] : []),
        expectedBuild(image, testsEnabled ?? 'true'),
      ]);
      const build = result.calls.find(call => call.kind === 'build');
      assert.equal(build.cacheWrite, 'false', 'master cache write permission must be overridden');
      assert.equal(build.testsEnabled, testsEnabled ?? null, 'explicit false must survive the build environment');
      assert.ok(result.calls.every(call => call.account === null), 'preview requires no Cloudflare account');
    });
  }
}

for (const scenario of [
  { image: 'sandbox', failure: 'prepare', expected: [{ kind: 'prepare', args: [] }] },
  { image: 'sandbox', failure: 'build', expected: [{ kind: 'prepare', args: [] }, expectedBuild('sandbox')] },
  { image: 'phone', failure: 'build', expected: [expectedBuild('phone')] },
]) {
  test(`${scenario.image} preview stops and propagates ${scenario.failure} failure`, t => {
    const result = fixture(t).run(scenario.image, { CI_TESTS_ENABLED: 'true', FIXTURE_FAIL_KIND: scenario.failure });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, new RegExp(`fixture failure: ${scenario.failure}`));
    assert.deepEqual(commands(result.calls), scenario.expected);
  });
}

test('preview rejects an unknown image before invoking any command', t => {
  const result = fixture(t).run('unknown-image');
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /unknown managed image/);
  assert.deepEqual(result.calls, []);
});
