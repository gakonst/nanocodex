import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const wrapper = fileURLToPath(new URL('./wrangler-docker.mjs', import.meta.url));
test('Wrangler Docker boundary preserves build inputs and isolates persistent cache scopes', () => {
  const dir = mkdtempSync(join(tmpdir(), 'cloudflare-docker-test-'));
  try {
    const capture = join(dir, 'capture.json');
    writeFileSync(join(dir, 'docker'), `#!${process.execPath}
const fs = require('node:fs');
fs.writeFileSync(process.env.CAPTURE, JSON.stringify({ args: process.argv.slice(2), stdin: fs.readFileSync(0, 'utf8') }));
process.exit(Number(process.env.DOCKER_EXIT || 0));
`, { mode: 0o755 });
    function invoke(args, input = '', overrides = {}) {
      const result = spawnSync(process.execPath, [wrapper, ...args], {
        input, encoding: 'utf8',
        env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CAPTURE: capture,
          GITHUB_WORKSPACE: '/runner/repo', BUILDX_BUILDER: 'ci-builder', ...overrides },
      });
      return { result, ...JSON.parse(readFileSync(capture, 'utf8')) };
    }
    // Exact argument shape used by installed Wrangler 4.127.1.
    const build = ['build', '--load', '-t', 'worker-sandbox:unique-tag', '--platform',
      'linux/amd64', '--provenance=false', '--build-arg', 'VALUE=a b', '-f', '-', '/runner/repo/js/managed'];
    const first = invoke(build, 'FROM scratch\nLABEL version=1\n');
    assert.equal(first.result.status, 0);
    assert.deepEqual(first.args.slice(0, 4), ['buildx', 'build', '--builder', 'ci-builder']);
    assert.deepEqual(first.args.slice(8), build.slice(1));
    assert.equal(first.stdin, 'FROM scratch\nLABEL version=1\n');
    assert.match(first.args[5], /^type=gha,version=2,scope=cloudflare-v1-/);
    assert.equal(first.args[7], `${first.args[5]},mode=max,timeout=3m,ignore-error=true`);
    const moved = invoke([...build.slice(0, -1), '/other/repo/js/managed'], 'FROM scratch\nLABEL version=2\n', { GITHUB_WORKSPACE: '/other/repo' });
    assert.equal(moved.args[5], first.args[5]);
    assert.equal(moved.stdin, 'FROM scratch\nLABEL version=2\n');
    const phone = invoke([...build.slice(0, -1), '/runner/repo']);
    assert.notEqual(phone.args[5], first.args[5]);
    for (const args of [['image', 'inspect', 'worker:tag'], ['login', '--password-stdin', 'registry.example'], ['tag', 'a', 'b'], ['push', 'b']]) {
      const passthrough = invoke(args, 'test-input', { DOCKER_EXIT: '17', BUILDX_BUILDER: '' });
      assert.deepEqual(passthrough.args, args);
      assert.equal(passthrough.stdin, 'test-input');
      assert.equal(passthrough.result.status, 17);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
