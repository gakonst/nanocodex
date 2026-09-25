#!/usr/bin/env node
// Provision and migrate before upload. Preview validation never needs a remote DB.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { writeFileSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { currentRelease } from './current-production-release.mjs';
import { accountValid, providerIdValid } from './live-worker-state.mjs';

const require = createRequire(new URL('../../js/managed/package.json', import.meta.url));
const binding = 'NANOCODEX_CRM';
const productionName = 'nanocodex-crm-production';
const previewName = 'nanocodex-crm-preview';
const allowedArgs = new Set(['--message', '--tag', '--containers-rollout']);

async function productionDatabase({ env, request, guard }) {
  assert.ok(accountValid(env.CLOUDFLARE_ACCOUNT_ID) && env.CLOUDFLARE_API_TOKEN,
    'CRM database deployment requires CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN');
  const base = `https://api.cloudflare.com/client/v4/accounts/${env.CLOUDFLARE_ACCOUNT_ID}/d1/database`;
  const call = async (url, method, body) => request(url, {
    method, redirect: 'error', signal: AbortSignal.timeout(30_000),
    headers: { Authorization: `Bearer ${env.CLOUDFLARE_API_TOKEN}`, 'Content-Type': 'application/json' },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  let stage = 'lookup-request';
  let status;
  try {
    // Wrangler 4.127.1 resolves names through this same endpoint. Only an actual
    // 404 permits creation; authorization/transient failures must stop release.
    let response = await call(`${base}/${productionName}`, 'GET');
    status = response.status;
    if (response.status === 404) {
      stage = 'create-guard';
      await guard();
      // Never retry an ambiguous creation. A later deployment looks up the name.
      stage = 'create-request';
      status = undefined;
      response = await call(base, 'POST', { name: productionName });
      status = response.status;
    }
    stage = stage === 'create-request' ? 'create-response' : 'lookup-response';
    assert.ok(response.ok);
    const body = await response.json();
    stage = stage.replace('-response', '-validation');
    assert.equal(body.success, true);
    assert.equal(body.result?.name, productionName);
    assert.ok(providerIdValid(body.result?.uuid));
    return body.result.uuid;
  } catch {
    // Only locally selected stages and numeric status are safe to log. Provider
    // bodies, request headers and exception causes may contain credentials.
    const http = Number.isInteger(status) && status >= 100 && status <= 599 ? status : 'unavailable';
    throw new Error(`CRM database lookup/create failed (stage=${stage}, HTTP=${http}); deployment stopped (no automatic retry)`);
  }
}

export async function deployManaged(mode, {
  config = 'wrangler.jsonc', env = process.env, request = globalThis.fetch,
  run = (command, args, options) => execFileSync(command, args, options),
  isCurrent = () => (env.GITHUB_ACTIONS === 'true' || env.GITHUB_EVENT_NAME) ? currentRelease() : true,
  deployArgs = [],
} = {}) {
  assert.ok(mode === 'deploy' || mode === 'preview', 'Expected deploy or preview mode');
  for (let i = 0; i < deployArgs.length; i += 2) {
    assert.ok(allowedArgs.has(deployArgs[i]) && typeof deployArgs[i + 1] === 'string'
      && !deployArgs[i + 1].startsWith('--'), 'Unsupported managed deployment argument');
  }
  const guard = async () => { if (!await isCurrent()) throw new Error('Managed production release superseded'); };
  if (mode === 'deploy') await guard();
  config = resolve(config);
  const { rawConfig } = require('wrangler').experimental_readRawConfig({ config });
  assert.equal(rawConfig.name, 'nanocodex-durable-agent', 'Expected managed Worker config');
  const databases = rawConfig.d1_databases?.filter(db => db.binding === binding);
  assert.equal(databases?.length, 1, 'Expected one CRM database binding');
  const database = databases[0];
  assert.equal(database.database_name, productionName, 'Expected production CRM database name');
  assert.equal(database.migrations_dir, 'migrations', 'Expected managed CRM migrations directory');
  const effective = structuredClone(rawConfig);
  // D1 bindings are non-inheritable. Explicitly remove named environments and
  // pass --env= so caller environment variables cannot switch the migration DB.
  delete effective.env;
  const childEnv = { ...env, CI: 'true', WRANGLER_SEND_METRICS: 'false' };
  let replacement;
  if (mode === 'deploy') {
    const id = await productionDatabase({ env, request, guard });
    assert.ok(!database.database_id || database.database_id === id, 'CRM database ID does not match its production name');
    replacement = { ...database, database_id: id };
  } else {
    replacement = { binding, database_name: previewName, database_id: previewName, migrations_dir: 'migrations', remote: false };
    for (const key of Object.keys(childEnv)) {
      if (/^(?:CLOUDFLARE_|CF_)/.test(key)) delete childEnv[key];
    }
  }
  effective.d1_databases = effective.d1_databases.map(db => db.binding === binding ? replacement : db);
  // Keep module, image and migration paths relative to the original config.
  const generated = join(dirname(config), `wrangler.crm-${randomUUID()}.jsonc`);
  writeFileSync(generated, JSON.stringify(effective, null, 2) + '\n', { flag: 'wx' });
  const options = { cwd: dirname(config), env: childEnv, stdio: 'inherit' };
  const target = ['--config', generated, '--env='];
  try {
    if (mode === 'deploy') await guard();
    await run('npx', ['wrangler', 'd1', 'migrations', 'apply', binding,
      mode === 'deploy' ? '--remote' : '--local', ...target], options);
    if (mode === 'deploy') await guard();
    await run('npx', ['wrangler', 'deploy', ...target,
      ...deployArgs, ...(mode === 'preview' ? ['--dry-run', '--containers-rollout', 'none'] : [])], options);
  } finally {
    rmSync(generated, { force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { values, positionals } = parseArgs({ allowPositionals: true, options: {
    config: { type: 'string', default: 'wrangler.jsonc' },
    message: { type: 'string' }, tag: { type: 'string' }, 'containers-rollout': { type: 'string' },
  } });
  assert.equal(positionals.length, 1, 'Usage: managed-crm.mjs deploy|preview [--config path]');
  const deployArgs = Object.entries(values).filter(([key]) => key !== 'config').flatMap(([key, value]) => [`--${key}`, value]);
  await deployManaged(positionals[0], { config: values.config, deployArgs });
}
