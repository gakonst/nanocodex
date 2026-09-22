import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const guard = fileURLToPath(new URL('./current-production-release.mjs', import.meta.url));
const workspace = fileURLToPath(new URL('../../', import.meta.url));
const workflow = readFileSync(new URL('../../.github/workflows/cloudflare.yml', import.meta.url), 'utf8');
const production = workflow.split('\n  production:\n')[1];
const oldSha = 'a'.repeat(40), newSha = 'b'.repeat(40);

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'production-guard-'));
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
      cwd: dir, env: { ...env, ...overrides }, encoding: 'utf8', input: '',
    }),
  };
}

function steps() {
  return production.split(/(?=^      - (?:name|uses):)/m).slice(1).map(source => {
    const name = /^      - name: (.+)$/m.exec(source)?.[1];
    const run = /^        run: (.+)$/m.exec(source)?.[1];
    const body = run === '|' ? source.split('        run: |\n')[1].split('\n')
      .filter(line => line.startsWith('          ')).map(line => line.slice(10)).join('\n') : run;
    return { name, source, run: body };
  });
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

test('workflow guards all ten deploys and secret mutation, preserving serial account-last order', () => {
  const all = steps();
  const mutations = all.filter(step => step.name?.startsWith('Deploy ') || step.name === 'Configure Astra trial secrets');
  assert.equal(mutations.length, 11);
  for (const step of mutations) {
    assert.equal((step.run.match(/current-production-release\.mjs" -- /g) ?? []).length, 1, step.name);
    assert.match(step.source, /if: steps\.current-release\.outputs\.active == 'true'/, step.name);
  }
  assert.equal((production.match(/current-production-release\.mjs/g) ?? []).length, 12); // early check plus eleven mutations
  assert.equal(mutations.at(-1).name, 'Deploy account application');
  assert.match(mutations.at(-1).source, /id: account-release/);
  assert.match(all.find(step => step.name === 'Verify deployed account Worker health').source,
    /if: steps\.account-release\.outputs\.active == 'true'/);
  assert.match(production, /group: cloudflare-production\n      cancel-in-progress: false/);
  assert.match(production, /GH_TOKEN: \$\{\{ github\.token \}\}/);
  assert.match(production, /DEPLOY_TARGET: \$\{\{ inputs\.target \}\}/);
  assert.doesNotMatch(production, /actions\/workflows\/cloudflare\.yml\/runs/);
  assert.ok(all.findIndex(step => step.name === 'Build agent email service') < all.indexOf(mutations[0]));
  assert.doesNotMatch(mutations.find(step => step.name === 'Deploy agent email service').run, /deploy:email|run build/);
});

test('actual workflow deployment commands preserve quoted message and empty env arguments', t => {
  const f = fixture(t);
  for (const step of steps().filter(step => step.name?.startsWith('Deploy '))) {
    const result = f.shell(step.run);
    assert.equal(result.status, 0, `${step.name}: ${result.stderr}`);
    const args = f.rows(f.deployments).at(-1).args;
    if (step.run.includes('--message')) assert.equal(args[args.indexOf('--message') + 1], f.env.DEPLOY_MESSAGE);
    if (step.name === 'Deploy Astra trial application') assert.ok(args.includes('--env='));
  }
  assert.equal(f.rows(f.deployments).length, 10);
  assert.equal(f.rows(f.queries).length, 10);
});

test('actual secret pipeline forwards JSON when current and cleanly skips when stale', t => {
  const f = fixture(t);
  const secrets = steps().find(step => step.name === 'Configure Astra trial secrets').run;
  const env = { ASTRA_MANAGED_API_KEY: 'synthetic "key"', ASTRA_MPP_SECRET: 'synthetic-secret', TEMPO_API_KEY: '' };
  let result = f.shell(secrets, env);
  assert.equal(result.status, 0, result.stderr);
  const [receipt] = f.rows(f.deployments);
  assert.deepEqual(receipt.args, ['wrangler', 'secret', 'bulk', '--env=']);
  assert.deepEqual(JSON.parse(receipt.input), { NANOCODEX_ASTRA_MANAGED_API_KEY: env.ASTRA_MANAGED_API_KEY,
    NANOCODEX_ASTRA_MPP_SECRET: env.ASTRA_MPP_SECRET });
  f.setHead(newSha);
  result = f.shell(secrets, env);
  assert.equal(result.status, 0, result.stderr);
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
