import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const guard = fileURLToPath(new URL('./current-production-release.mjs', import.meta.url));
const workspace = fileURLToPath(new URL('../../', import.meta.url));
const oldSha = 'a'.repeat(40), newSha = 'b'.repeat(40);

function fixture(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'production-guard-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bin = join(dir, 'bin');
  mkdirSync(bin);
  const head = join(dir, 'head.json'), queries = join(dir, 'queries.jsonl');
  const deployments = join(dir, 'deployments.jsonl'), output = join(dir, 'output');
  const executable = (name, source) => writeFileSync(join(bin, name), `#!/usr/bin/env node\n${source}`, { mode: 0o755 });
  executable('gh', `
    const fs = require('node:fs');
    fs.appendFileSync(process.env.QUERIES, JSON.stringify(process.argv.slice(2)) + '\\n');
    if (process.env.GH_FAILURE) { console.error('synthetic-private-error'); process.exit(22); }
    process.stdout.write(fs.readFileSync(process.env.HEAD_FILE));
  `);
  const deploy = `
    const fs = require('node:fs');
    fs.appendFileSync(process.env.DEPLOYMENTS, JSON.stringify({
      args: process.argv.slice(2), cwd: process.cwd(), input: fs.readFileSync(0, 'utf8'),
    }) + '\\n');
    process.exit(Number(process.env.DEPLOY_EXIT || 0));
  `;
  for (const name of ['deploy', 'npx', 'pnpm']) executable(name, deploy);
  const env = { ...process.env, PATH: bin + delimiter + process.env.PATH,
    GITHUB_EVENT_NAME: 'push', GITHUB_REF: 'refs/heads/master', GITHUB_REPOSITORY: 'fixture/repository',
    GITHUB_SHA: oldSha, GITHUB_OUTPUT: output, GITHUB_WORKSPACE: workspace,
    DEPLOY_TARGET: '', GH_FAILURE: '', DEPLOY_EXIT: '',
    GH_TOKEN: 'synthetic-token', HEAD_FILE: head, QUERIES: queries, DEPLOYMENTS: deployments,
    DEPLOY_MESSAGE: 'release with spaces "quotes" $HOME `literal` $(literal)',
  };
  const setHead = sha => writeFileSync(head, JSON.stringify({ object: { type: 'commit', sha } }));
  setHead(oldSha);
  const rows = path => existsSync(path) ? readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
  const run = (args = [], overrides = {}, input = '') => {
    writeFileSync(output, '');
    return spawnSync(process.execPath, [guard, ...args], { cwd: dir, env: { ...env, ...overrides }, input, encoding: 'utf8' });
  };
  return { dir, env, head, setHead, run, rows, queries, deployments,
    output: () => readFileSync(output, 'utf8'),
    shell: (source, overrides = {}) => spawnSync('bash', ['-e', '-o', 'pipefail', '-c', source], {
      cwd: workspace, env: { ...env, ...overrides }, encoding: 'utf8', input: '',
    }),
  };
}

test('matching master authorizes command and preserves literal argv, cwd and stdin', t => {
  const f = fixture(t);
  const args = ['--message', 'spaces "quotes" $HOME `literal` $(literal)', '--env=', 'line\nbreak'];
  const result = f.run(['--', 'deploy', ...args], {}, 'synthetic stdin');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.output(), 'active=true\n');
  assert.deepEqual(f.rows(f.deployments), [{ args, cwd: f.dir, input: 'synthetic stdin' }]);
  assert.deepEqual(f.rows(f.queries), [['api', 'repos/fixture/repository/git/ref/heads/master', '--header', 'Cache-Control: no-cache']]);
});

test('a master push after the early/build check skips deployment with exit zero', t => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  assert.equal(f.output(), 'active=true\n');
  // The job built its old checkout while master advanced, with no new workflow
  // run metadata yet. Only the live branch head is authoritative.
  f.setHead(newSha);
  const result = f.run(['--', 'deploy']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.output(), 'active=false\n');
  assert.match(result.stdout, /Skipping superseded production push/);
  assert.deepEqual(f.rows(f.deployments), []);
  assert.equal(f.rows(f.queries).length, 2);
});

test('rechecks between components prevent account-last deployment after a newer push', t => {
  const f = fixture(t);
  assert.equal(f.run(['--', 'deploy', 'egress']).status, 0);
  f.setHead(newSha);
  for (const stage of ['managed', 'secrets', 'account']) {
    assert.equal(f.run(['--', 'deploy', stage]).status, 0);
    assert.equal(f.output(), 'active=false\n');
  }
  assert.deepEqual(f.rows(f.deployments).map(x => x.args), [['egress']]);
  assert.equal(f.rows(f.queries).length, 4);
});

test('explicit production dispatch permits rollback without consulting master', t => {
  const f = fixture(t);
  f.setHead(newSha);
  const result = f.run(['--', 'deploy', 'rollback'], { GITHUB_EVENT_NAME: 'workflow_dispatch',
    GITHUB_REF: 'refs/tags/old-release', DEPLOY_TARGET: 'production', GH_FAILURE: '1' });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.output(), 'active=true\n');
  assert.equal(f.rows(f.deployments).length, 1);
  assert.deepEqual(f.rows(f.queries), []);
});

test('lookup failure and malformed master responses fail closed without private output', t => {
  const f = fixture(t);
  for (const response of [null, 'not json', '{}', JSON.stringify({ object: { type: 'tag', sha: oldSha } }),
    JSON.stringify({ object: { type: 'commit', sha: 'short' } })]) {
    if (response !== null) writeFileSync(f.head, response);
    const result = f.run(['--', 'deploy'], { GH_FAILURE: response === null ? '1' : '' });
    assert.equal(result.status, 1);
    assert.equal(f.output(), '');
    assert.doesNotMatch(result.stdout + result.stderr, /synthetic-private-error|synthetic-token/);
  }
  assert.deepEqual(f.rows(f.deployments), []);
});

test('unknown events, preview dispatch, nonmaster push and invalid identity fail closed', t => {
  const f = fixture(t);
  for (const overrides of [
    { GITHUB_EVENT_NAME: 'pull_request' },
    { GITHUB_EVENT_NAME: 'workflow_dispatch', DEPLOY_TARGET: 'preview' },
    { GITHUB_EVENT_NAME: 'workflow_dispatch' },
    { GITHUB_REF: 'refs/heads/topic' }, { GITHUB_SHA: 'short' }, { GITHUB_REPOSITORY: '../bad' },
  ]) assert.equal(f.run(['--', 'deploy'], overrides).status, 1);
  assert.deepEqual(f.rows(f.deployments), []);
  assert.deepEqual(f.rows(f.queries), []);
});

test('command failure propagates and missing command cannot report success', t => {
  const f = fixture(t);
  assert.equal(f.run(['--', 'deploy'], { DEPLOY_EXIT: '17' }).status, 17);
  assert.equal(f.run(['--', 'nonexistent-synthetic-command']).status, 1);
  assert.equal(f.run(['--']).status, 1);
  assert.equal(f.run(['deploy']).status, 1);
  assert.equal(f.rows(f.deployments).length, 1);
});

test('stale piped mutation drains large input so pipefail remains a successful skip', t => {
  const f = fixture(t);
  f.setHead(newSha);
  const result = f.shell(`node -e 'process.stdout.write("x".repeat(1024 * 1024))' | node "$GITHUB_WORKSPACE/scripts/cloudflare/current-production-release.mjs" -- deploy`);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.rows(f.deployments), []);
});

test('repository and both local/API commit SHAs reject trailing newlines', t => {
  const f = fixture(t);
  for (const overrides of [{ GITHUB_REPOSITORY: 'fixture/repository\n' }, { GITHUB_SHA: oldSha + '\n' }]) {
    assert.equal(f.run(['--', 'deploy'], overrides).status, 1);
  }
  assert.deepEqual(f.rows(f.queries), []);
  f.setHead(oldSha + '\n');
  assert.equal(f.run(['--', 'deploy']).status, 1);
  assert.deepEqual(f.rows(f.deployments), []);
});
