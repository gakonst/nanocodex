#!/usr/bin/env node
// Executes the existing black-box HTTP journey with source package placement
// aliases only. Does not modify the shipped routes, auth, SQLite, or proxy.
// External Google/broker are explicit synthetic fixtures from the original test.
import { readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { join } from 'node:path';
const managed = fileURLToPath(new URL('..', import.meta.url));
const output = fileURLToPath(new URL('../../../output/mobile-decision-inbox/e2e', import.meta.url));
await mkdir(output, { recursive: true });
const temporary = join(managed, 'scripts', '.decision-inbox-existing-journey-' + crypto.randomUUID() + '.mjs');
const aliases = Object.fromEntries(['rpc', 'managed-auth', 'managed-live', 'managed-access'].map(name => [
  'nanocodex/cloudflare/' + name, fileURLToPath(new URL('../../nanocodex/cloudflare/' + name + '.mjs', import.meta.url)),
]));
let test = await readFile(new URL('../test/todo-mail-journey.test.mjs', import.meta.url), 'utf8');
const marker = 'alias:{"node-rsa":';
if (!test.includes(marker)) throw Error('existing journey build shape changed; inspect before running');
test = test.replace(marker, 'alias:{' + Object.entries(aliases).map(([k, v]) => JSON.stringify(k) + ':' + JSON.stringify(v)).join(',') + ',"node-rsa":');
test = test.replace('output/todo-mail-http-journey.json', 'output/mobile-decision-inbox/e2e/local-existing-http-trace.json');
await writeFile(temporary, test, { flag: 'wx' });
try {
  const start = performance.now(), chunks = [];
  const child = spawn(process.execPath, ['--test', temporary], { cwd: managed, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.on('data', d => chunks.push(d)); child.stderr.on('data', d => chunks.push(d));
  const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', resolve); });
  await writeFile(join(output, 'local-existing-http-journey.log'), Buffer.concat(chunks));
  const result = { command: 'node js/managed/scripts/decision-inbox-local-journey.mjs', exit_code: code, elapsed_ms: Number((performance.now() - start).toFixed(2)),
    auth: 'Real shipped authenticate() plus real createApiKey(); synthetic owner and credential exist only in local worker.',
    transport: 'Real loopback HTTP -> shipped account proxy -> shipped TODO router -> SQLite Durable Object',
    external_fixture: 'Google provider and connector broker synthetic. Fixture sends do not reach real Google or mailbox.',
    module_resolution: 'Source module aliases correct stale linked dependency package placement only; no route behavior stubs.',
    feature_coverage: 'Existing draft/send/replay/stale/race/unknown paths only; new async preparation coverage separately required.',
    report: 'output/mobile-decision-inbox/e2e/local-existing-http-trace.json' };
  await writeFile(join(output, 'local-existing-http-result.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(JSON.stringify(result, null, 2));
  if (code !== 0) process.exitCode = code ?? 1;
} finally { await rm(temporary, { force: true }); }
