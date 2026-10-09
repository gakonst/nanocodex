#!/usr/bin/env node
// Native branch Previews: no production deployment mutations or secret copying.
import { execFileSync, spawn } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve, relative } from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { deployedAccountImages } from './released-account-image.mjs';

const require = createRequire(import.meta.url);
export const components = {
  managed: { worker: 'nanocodex-durable-agent', config: 'js/managed/wrangler.jsonc' },
  'connect-api': { worker: 'nanocodex-connect-api', config: 'js/connect-api/wrangler.jsonc' },
  dialog: { worker: 'nanocodex-connect-dialog', config: 'js/connect-dialog/wrangler.jsonc' },
  playground: { worker: 'nanocodex-connect-playground', config: 'js/connect-playground/wrangler.jsonc' },
  account: { worker: 'nanocodex', config: 'js/account/wrangler.jsonc', built: 'js/account/dist/nanocodex/wrangler.json' },
};
const copied = ['vars', 'services', 'durable_objects', 'containers', 'ai', 'browser', 'images', 'stream', 'media',
  'worker_loaders', 'version_metadata', 'ratelimits', 'observability', 'limits', 'placement'];
// These bindings MUST already point to separate, explicitly provisioned Base resources.
const storage = [
  ['r2_buckets', 'r2_bucket', 'bucket_name'], ['d1_databases', 'd1', 'database_id'],
  ['kv_namespaces', 'kv_namespace', 'namespace_id'], ['ai_search', 'ai_search', 'instance_name'],
];
const safeName = value => typeof value === 'string' && /^[A-Za-z0-9_.-]+$/.test(value);
const fail = message => { throw new Error(message); };
const secretNames = env => Object.entries(env ?? {}).filter(([, value]) => value?.type === 'secret_text' || value?.type === 'secret_key').map(([name]) => name).sort();
function readConfig(path) { return require('wrangler').experimental_readRawConfig({ config: path }).rawConfig; }

export function selectComponents(component, backend = 'production') {
  if (!component || component === 'all') return backend === 'production' ? ['dialog', 'playground', 'account'] : Object.keys(components);
  if (component === 'assets') return ['dialog', 'playground'];
  if (!Object.hasOwn(components, component)) fail('Unknown preview component; use managed, connect-api, dialog, playground, account, or assets');
  return [component];
}

export function providerClient({ account, token, request = globalThis.fetch }) {
  if (!/^[a-f0-9]{32}$/.test(account ?? '') || !token) fail('Cloudflare account ID and API token are required');
  return async (path, { optional = false, method = 'GET', body: payload } = {}) => {
    let status;
    try {
      const response = await request(`https://api.cloudflare.com/client/v4/accounts/${account}/${path}`, {
        method, ...(payload ? { body: JSON.stringify(payload) } : {}), redirect: 'error', signal: AbortSignal.timeout(30_000),
        headers: { Authorization: `Bearer ${token}`, 'Cache-Control': 'no-cache', ...(payload ? { 'Content-Type': 'application/merge-patch+json' } : {}) },
      });
      status = response.status;
      if (optional && status === 404) return null;
      if (!response.ok) throw new Error();
      const body = await response.json();
      if (body.success !== true || (body.result === undefined && method !== 'DELETE')) throw new Error();
      return body.result;
    } catch { fail(`Cloudflare preview metadata lookup failed (HTTP ${Number.isInteger(status) ? status : 'unavailable'}); no response body logged`); }
  };
}

export async function checkComponent(component, config, { get, name, suppliedNames = [], backend = 'isolated' }) {
  const worker = components[component].worker;
  const parent = `workers/workers/${worker}`;
  const [production, base, existing] = await Promise.all([
    backend === 'production' ? Promise.resolve([]) : get(`workers/scripts/${worker}/secrets`), get(parent), get(`${parent}/previews/${name}`, { optional: true }),
  ]);
  if (!Array.isArray(production)) fail(`Invalid production secret metadata for ${worker}`);
  const required = backend === 'production' ? [] : production.map(item => item.name).sort();
  if (required.some(key => Object.hasOwn(config.vars ?? {}, key))) fail(`Production secret name collides with configured plain variables for ${worker}`);
  if (!required.every(safeName)) fail(`Invalid secret name metadata for ${worker}`);
  const baseEnv = base.previews_base_config?.env ?? {};
  const baseNames = new Set([...secretNames(baseEnv), ...suppliedNames]);
  const missing = required.filter(key => !baseNames.has(key));
  const problems = [];
  if (base.subdomain?.previews_enabled === false) problems.push('workers.dev Preview URLs must be enabled on the parent Worker before deployment');
  if (missing.length) problems.push(`missing Preview Base secrets: ${missing.join(', ')}`);
  let activeEnv;
  if (existing) {
    const deployed = await get(`${parent}/previews/${name}/deployments/latest`, { optional: true });
    activeEnv = deployed?.env;
    const needsBindingMetadata = required.length > 0 || backend !== 'production' && storage.some(([field]) => (config[field]?.length ?? 0) > 0);
    if (deployed && !activeEnv && needsBindingMetadata) fail(`Existing Preview binding metadata unavailable for ${worker}`);
    if (deployed && !activeEnv) activeEnv = {};
    if (activeEnv) {
      const activeNames = new Set(secretNames(activeEnv));
      const absent = required.filter(key => !activeNames.has(key));
      if (absent.length) problems.push(`existing Preview missing secrets (Base changes are not retroactive): ${absent.join(', ')}`);
    }
  }
  const resources = [];
  for (const [field, type, property] of backend === 'production' ? [] : storage) {
    for (const binding of config[field] ?? []) {
      const key = binding.binding;
      if (!safeName(key)) fail(`Invalid resource binding name for ${worker}`);
      const candidate = baseEnv[key];
      const productionValue = binding[property] ?? binding.database_id ?? binding.id;
      const value = candidate?.[property];
      // Missing production D1 IDs are resolved by database name, read-only.
      let original = productionValue;
      if (type === 'd1' && !original && candidate?.type === type && candidate?.[property]) {
        const databases = await get(`d1/database?name=${encodeURIComponent(binding.database_name)}&per_page=100`);
        const matches = Array.isArray(databases) ? databases.filter(db => db.name === binding.database_name) : [];
        if (matches.length !== 1 || !matches[0].uuid) fail(`Cannot identify production D1 boundary for ${worker}/${key}`);
        original = matches[0].uuid;
      }
      if (!candidate || candidate.type !== type || typeof value !== 'string' || !value || value === original) {
        problems.push(`separate Preview Base resource required: ${key}`);
      } else if (activeEnv && (activeEnv[key]?.type !== type || activeEnv[key]?.[property] !== value)) {
        problems.push(`existing Preview resource differs from Base: ${key}`);
      }
      resources.push({ binding: key, type, scope: 'preprovisioned-preview-base' });
    }
  }
  // New storage kinds must receive an explicit policy before becoming preview bindings.
  for (const field of ['queues', 'workflows', 'vectorize', 'hyperdrive', 'analytics_engine_datasets', 'pipelines', 'dispatch_namespaces']) {
    if (backend !== 'production' && config[field] && JSON.stringify(config[field]) !== '[]') problems.push(`preview policy required for binding category: ${field}`);
  }
  return { component, worker, requiredSecrets: required, missingBaseSecrets: missing, plannedBaseSecretNames: suppliedNames, problems, resources };
}

export function previewConfig(source, { revision, images = [], bridge, backend = 'isolated' } = {}) {
  const config = structuredClone(source);
  delete config.env;
  delete config.$schema;
  // Start explicitly; inherited production routes/cron/queue consumers are never deployed.
  config.previews = {};
  if (backend === 'production' && source.name === components.account.worker) {
    // All stateful requests use the existing authenticated production API.
    // Only the branch's app rendering and assets run in this Preview.
    if (config.assets) config.assets.run_worker_first = true;
    // The Preview owns no Durable Object namespaces: no migrations, owned DO bindings or containers.
    // Wrangler uploads migrations from the top level even though Preview bindings come only from previews.
    if (config.exports && Object.keys(config.exports).length) fail('Production-backed account Preview cannot declare Worker exports');
    delete config.exports;
    delete config.migrations;
    delete config.containers;
    delete config.durable_objects;
    config.previews = { vars: { ENVIRONMENT: 'production', NANOCODEX_PREVIEW_REVISION: revision },
      services: [{ binding: 'NANOCODEX_PREVIEW_PRODUCTION', service: components.account.worker }] };
    return config;
  }
  for (const key of copied) if (source[key] !== undefined) config.previews[key] = structuredClone(source[key]);
  config.previews.vars = { ...config.previews.vars, NANOCODEX_PREVIEW_REVISION: revision };
  if (bridge) {
    Object.assign(config.previews.vars, {
      NANOCODEX_PREVIEW_MANAGED_URL: bridge.managed,
      NANOCODEX_PREVIEW_ACCOUNT_ORIGIN: bridge.account,
    });
    if (source.name === components.account.worker) {
      const omitted = new Set(['NANOCODEX_HAND_BROKER', 'NANOCODEX_LIVE_API_KEYS', 'NANOCODEX_LIVE_SESSIONS']);
      config.previews.durable_objects.bindings = config.previews.durable_objects.bindings.filter(binding => !omitted.has(binding.name));
    }
  }
  // Use local DO namespace isolation. Foreign bindings remain explicit production boundaries.
  if (config.previews.containers) {
    if (images.length !== config.previews.containers.length) fail('Published container images are required for every preview container');
    config.previews.containers.forEach((container, index) => {
      container.image = images[index];
      delete container.image_build_context;
      delete container.image_vars;
      delete container.name;
    });
  }
  // Preview storage comes only from the preflighted server-side Base, never production config.
  return config;
}

function boundaries(config) {
  return {
    ...(config.previews.vars?.NANOCODEX_PREVIEW_MANAGED_URL ? { httpBridge: { managed: config.previews.vars.NANOCODEX_PREVIEW_MANAGED_URL, account: config.previews.vars.NANOCODEX_PREVIEW_ACCOUNT_ORIGIN } } : {}),
    services: (config.previews.services ?? []).map(({ binding, service, entrypoint }) => ({ binding, service, ...(entrypoint ? { entrypoint } : {}), target: binding === 'NANOCODEX_BACKEND' && config.previews.vars?.NANOCODEX_PREVIEW_MANAGED_URL ? 'preview-http-bridge' : 'production' })),
    foreignDurableObjects: (config.previews.durable_objects?.bindings ?? []).filter(item => item.script_name)
      .map(({ name, class_name, script_name }) => ({ binding: name, class: class_name, worker: script_name, target: 'production' })),
  };
}
// Fixed failure labels only. Raw Wrangler lines can contain binding values, so they are never emitted.
const failureCategories = [
  ['cloudflare-api-request', /A request to the Cloudflare API/i], ['authentication', /authenticate|authentication|authenticating|unauthori[sz]ed|forbidden/i],
  ['migrations', /migration/i], ['durable-objects', /durable object/i], ['containers', /container/i], ['docker', /docker/i],
  ['exports', /export/i], ['bindings', /binding/i], ['bundling', /build failed|could not resolve|esbuild/i],
  ['configuration', /configuration|unexpected field|is not a valid/i], ['size-limit', /too large|exceeds|size limit/i],
  ['network', /ECONNRESET|ECONNREFUSED|ETIMEDOUT|ENOTFOUND|fetch failed/i], ['node-version', /requires at least Node/i],
];
const ansi = new RegExp(String.raw`\u001b\[[0-9;]*[A-Za-z]`, 'g');
// Streams stderr through a bounded window; memory stays constant regardless of output size.
export function failureClassifier({ window = 512, maxCodes = 16 } = {}) {
  let carry = '';
  const codes = new Set(); const categories = new Set();
  return {
    push(chunk) {
      const text = (carry + String(chunk)).replace(ansi, '');
      for (const match of text.matchAll(/\[code: ([0-9]{1,9})\]/g)) if (codes.size < maxCodes) codes.add(Number(match[1]));
      for (const [label, pattern] of failureCategories) if (pattern.test(text)) categories.add(label);
      carry = text.slice(-window);
    },
    summary() {
      return { codes: [...codes].sort((a, b) => a - b), categories: failureCategories.map(([label]) => label).filter(label => categories.has(label)) };
    },
  };
}
export function failureMessage(code, { codes, categories }) {
  return `Wrangler Preview failed (exit ${code}); Cloudflare API codes: ${codes.length ? codes.join(', ') : 'none'}; categories: ${categories.length ? categories.join(', ') : 'unclassified'}; raw output withheld to protect binding values`;
}
export function runWrangler(args, { cwd, env, secrets, bin = resolve(dirname(require.resolve('wrangler/package.json')), 'bin/wrangler.js') }) {
  return new Promise((accept, reject) => {
    const child = spawn(process.execPath, [bin, ...args], {
      cwd, env, stdio: [secrets ? 'pipe' : 'ignore', 'ignore', 'pipe'],
    });
    const classifier = failureClassifier();
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', chunk => classifier.push(chunk));
    if (secrets) { child.stdin.on('error', () => {}); child.stdin.end(JSON.stringify(secrets)); }
    child.once('error', () => reject(new Error('Wrangler Preview process could not start')));
    child.once('close', code => code === 0 ? accept() : reject(new Error(failureMessage(code, classifier.summary()))));
  });
}
function checkedUrls(urls) {
  if (!Array.isArray(urls)) return [];
  return urls.map(value => {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.pathname !== '/') fail('Invalid Preview URL receipt');
    return url.origin;
  });
}

// Read-only propagation retries, bounded to 30 seconds per deployed Worker.
export async function smokePreview(component, origin, { request = globalThis.fetch } = {}) {
  const probes = component === 'account'
    ? [{ path: '/', statuses: [200], html: true }, { path: '/v1/credentials', statuses: [401, 403] }]
    : ['dialog', 'playground'].includes(component)
      ? [{ path: '/', statuses: [200], html: true }]
      : component === 'managed' ? [{ path: '/', statuses: [403] }] : [];
  const deadline = Date.now() + 30_000;
  const results = [];
  for (const probe of probes) {
    let observed = { path: probe.path, status: null, passed: false };
    while (Date.now() < deadline) {
      try {
        const response = await request(new URL(probe.path, origin), {
          method: 'GET', redirect: 'manual', signal: AbortSignal.timeout(Math.max(1, Math.min(5000, deadline - Date.now()))),
          headers: { Accept: probe.html ? 'text/html' : 'application/json', 'Cache-Control': 'no-cache' },
        });
        const html = /^text\/html(?:;|$)/i.test(response.headers.get('content-type') ?? '');
        observed = { path: probe.path, status: response.status, ...(probe.html ? { html } : {}),
          passed: probe.statuses.includes(response.status) && (!probe.html || html) };
        await response.body?.cancel();
        if (observed.passed) break;
      } catch { /* Never retain response bodies or fetch exceptions. */ }
      const remaining = deadline - Date.now();
      if (remaining > 0) await new Promise(done => setTimeout(done, Math.min(1000, remaining)));
    }
    results.push(observed);
    if (!observed.passed) break;
  }
  return { passed: results.length === probes.length && results.every(result => result.passed), probes: results,
    scope: probes.length ? 'unauthenticated-serving-check' : 'receipt-only' };
}

export async function main(args = process.argv.slice(2), { cwd = process.cwd(), env = process.env, request = globalThis.fetch, run = runWrangler } = {}) {
  const command = args[0] && !args[0].startsWith('--') ? args[0] : 'deploy';
  if (!['deploy', 'check', 'delete'].includes(command)) fail('Expected deploy, check, or delete');
  const { values } = parseArgs({ args: args[0] === command ? args.slice(1) : args, options: {
    backend: { type: 'string', default: 'production' }, 'check-only': { type: 'boolean', default: false }, component: { type: 'string' }, name: { type: 'string' },
  } });
  if (command === 'check') values['check-only'] = true;
  const name = values.name ?? env.PREVIEW_NAME;
  if (!/^pr-[1-9][0-9]*$/.test(name ?? '')) fail('Pass --name pr-N or PREVIEW_NAME=pr-N');
  const revision = env.GITHUB_SHA ?? env.PREVIEW_REVISION;
  if (command !== 'delete' && !/^[a-f0-9]{40}$/.test(revision ?? '')) fail('GITHUB_SHA or PREVIEW_REVISION must identify the full checkout revision');
  const head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
  if (command !== 'delete' && revision !== head) fail('Preview revision does not match checkout HEAD');
  const version = require('wrangler/package.json').version.split('.').map(Number);
  if (version[0] < 4 || (version[0] === 4 && version[1] < 135)) fail('Native Worker Previews require root Wrangler >=4.135.0');
  const backend = values.backend;
  if (!['production', 'isolated'].includes(backend)) fail('Expected --backend production or isolated');
  const selected = command === 'delete' && !values.component ? Object.keys(components) : selectComponents(values.component, backend);
  if (command !== 'delete' && backend === 'production' && selected.some(key => ['managed', 'connect-api'].includes(key))) fail('Managed/Connect API branch code requires --backend isolated');
  // Account/managed must roll together so a rotated bridge key cannot strand one side.
  if (backend === 'isolated' && (selected.includes('account') || selected.includes('managed'))) {
    if (!selected.includes('managed')) selected.unshift('managed');
    if (!selected.includes('account')) selected.push('account');
  }
  const account = env.CLOUDFLARE_ACCOUNT_ID ?? env.CF_ACCOUNT_ID;
  const token = env.CLOUDFLARE_API_TOKEN ?? env.CF_API_TOKEN ?? env.CF_TOKEN;
  const get = providerClient({ account, token, request });
  if (command === 'delete') {
    const removed = [];
    const output = resolve(cwd, 'output/cloudflare-previews', name);
    mkdirSync(output, { recursive: true });
    for (const key of [...selected].reverse()) {
      const worker = components[key].worker;
      const path = `workers/workers/${worker}/previews/${name}`;
      const existing = await get(path, { optional: true });
      if (existing) await get(path, { method: 'DELETE' });
      if (await get(path, { optional: true })) fail(`Preview deletion could not be verified: ${worker}`);
      removed.push({ worker, name, deleted: Boolean(existing), absent: true });
      writeFileSync(resolve(output, 'deletion.json'), JSON.stringify({ schema: 1, name, removed }, null, 2) + '\n');
      console.log(`Verified Preview absent: ${worker}/${name}`);
    }
    if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `\nPreview cleanup verified: ${name} (${removed.length} Workers).\n`);
    return { name, removed };
  }
  let supplied = {};
  if (backend === 'isolated' && env.NANOCODEX_PREVIEW_BASE_SECRETS_JSON) {
    try { supplied = JSON.parse(env.NANOCODEX_PREVIEW_BASE_SECRETS_JSON); } catch { fail('Invalid NANOCODEX_PREVIEW_BASE_SECRETS_JSON; expected worker-name to secret-name/value maps'); }
    if (!supplied || typeof supplied !== 'object' || Array.isArray(supplied)) fail('Invalid Preview Base secret seed shape');
  }
  // Optional private provisioning; never retrieve production secret values.
  // Validate the complete selected seed before any mutation, with name-only metadata.
  const seedPlans = [];
  for (const key of selected) {
    const worker = components[key].worker;
    const secrets = supplied[worker];
    if (secrets === undefined) continue;
    if (!secrets || typeof secrets !== 'object' || Array.isArray(secrets)) fail(`Invalid Preview Base seed for ${worker}`);
    const metadata = await get(`workers/scripts/${worker}/secrets`);
    if (!Array.isArray(metadata)) fail(`Invalid production secret metadata for ${worker}`);
    const allowed = new Set(metadata.map(item => item.name));
    for (const [name, value] of Object.entries(secrets)) {
      if (!safeName(name) || !allowed.has(name) || typeof value !== 'string' || !value) fail(`Invalid Preview Base seed entries for ${worker}; only existing production secret names and nonempty values are allowed`);
    }
    seedPlans.push({ worker, secrets });
  }
  if (!values['check-only']) for (const { worker, secrets } of seedPlans) {
    await get(`workers/workers/${worker}`, { method: 'PATCH', body: {
      previews_base_config: { env: Object.fromEntries(Object.entries(secrets).map(([key, text]) => [key, { type: 'secret_text', text }])) },
    } });
    console.log(`Privately provisioned Preview Base secrets: ${worker}`);
  }
  const configs = Object.fromEntries(selected.map(key => [key, readConfig(resolve(cwd, components[key].config))]));
  const checks = [];
  for (const key of selected) {
    console.log(`Checking Preview Base metadata: ${components[key].worker}`);
    checks.push(await checkComponent(key, configs[key], { get, name, backend, suppliedNames: values['check-only'] ? Object.keys(supplied[components[key].worker] ?? {}) : [] }));
  }
  const manifest = { schema: 1, name, revision, backend, credentialBroker: backend === 'production' ? 'existing-production-via-account-service' : 'production-egress', mode: values['check-only'] ? 'check-only' : 'deploy', checks, deployments: [] };
  const output = resolve(cwd, 'output/cloudflare-previews', name);
  mkdirSync(output, { recursive: true });
  const manifestPath = resolve(output, 'manifest.json');
  const save = () => writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  save();
  writeFileSync(resolve(output, 'base-secrets-missing.json'), JSON.stringify({ name, revision, workers: checks.map(({ worker, missingBaseSecrets }) => ({ worker, missing: missingBaseSecrets })) }, null, 2) + '\n');
  const problems = checks.flatMap(check => check.problems.map(problem => `${check.worker}: ${problem}`));
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY,
    `\n## Worker Preview ${name}\n\nRevision: \`${revision}\`\n\n${problems.length ? problems.map(problem => `- ${problem}`).join('\n') : 'Preview Base metadata checks passed.'}\n`);
  if (problems.length) fail(`Preview preflight blocked before uploads:\n${problems.join('\n')}\nProvision missing Base secrets privately and separate storage explicitly; secret values are never copied from production.`);
  if (values['check-only']) return manifest;
  let bridge;
  if (backend === 'isolated' && selected.includes('account')) {
    const domain = await get('workers/subdomain');
    if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(domain?.subdomain ?? '')) fail('Invalid Workers subdomain metadata');
    bridge = { account: `https://${name}-${components.account.worker}.${domain.subdomain}.workers.dev`,
      managed: `https://${name}-${components.managed.worker}.${domain.subdomain}.workers.dev`, secret: randomBytes(32).toString('hex') };
  }
  for (const key of selected) {
    const spec = components[key];
    const original = resolve(cwd, spec.built ?? spec.config);
    if (!existsSync(original)) fail(`Missing built configuration for ${key}; restore same-revision Worker artifacts first`);
    const source = readConfig(original);
    if (source.name !== spec.worker) fail(`Unexpected built Worker identity for ${key}`);
    const images = backend === 'isolated' && source.containers?.length ? await deployedAccountImages(source, { account, token, request }) : [];
    const usesBridge = bridge && ['account', 'managed'].includes(key);
    const config = previewConfig(source, { revision, images, backend, bridge: usesBridge ? bridge : undefined });
    const generated = resolve(dirname(original), `wrangler.preview-${randomUUID()}.json`);
    const receipt = resolve(output, `wrangler-${randomUUID()}.jsonl`);
    const entry = backend === 'production' && key === 'account' ? resolve(dirname(original), `preview-entry-${randomUUID()}.mjs`) : undefined;
    try {
      if (entry) {
        const appPath = './' + relative(dirname(entry), resolve(dirname(original), source.main));
        const helperPath = './' + relative(dirname(entry), resolve(cwd, 'scripts/cloudflare/production-preview-entry.ts'));
        writeFileSync(entry, `import app from ${JSON.stringify(appPath)};\nexport * from ${JSON.stringify(appPath)};\nimport { productionPreviewFetch } from ${JSON.stringify(helperPath)};\nexport default { ...app, fetch(request, env, ctx) { return productionPreviewFetch(request, env, ctx, (r, e, c) => app.fetch(r, e, c)); } };\n`, { flag: 'wx', mode: 0o600 });
        config.main = './' + relative(dirname(original), entry);
        config.no_bundle = false;
      }
      writeFileSync(generated, `${JSON.stringify(config, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
      console.log(`Deploying native Preview ${name}: ${spec.worker}`);
      await run(['preview', '--env=', '--config', generated, '--name', name, '--tag', revision, '--message', `PR preview ${revision}`, '--json', ...(backend === 'production' ? ['--ignore-base-config'] : []), ...(usesBridge ? ['--secrets-file', '/dev/stdin'] : [])], {
        secrets: usesBridge ? { NANOCODEX_PREVIEW_BRIDGE_SECRET: bridge.secret } : undefined,
        cwd, env: { ...env, CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, NANOCODEX_PREVIEW_BASE_SECRETS_JSON: '', CI: 'true', WRANGLER_SEND_METRICS: 'false', WRANGLER_OUTPUT_FILE_PATH: receipt },
      });
      const entries = readFileSync(receipt, 'utf8').trim().split('\n').map(line => JSON.parse(line));
      const result = entries.filter(entry => entry.type === 'preview' && entry.worker_name === spec.worker && entry.preview_name === name).at(-1);
      if (!result || !safeName(result.preview_id) || !safeName(result.deployment_id)) fail(`Missing native Preview deployment receipt for ${spec.worker}`);
      const urls = checkedUrls(result.preview_urls);
      const deploymentUrls = checkedUrls(result.deployment_urls);
      if (usesBridge && !urls.includes(bridge[key])) fail(`Preview URL differs from authenticated bridge origin: ${spec.worker}`);
      if (!urls.length) fail(`Preview has no public URL: ${spec.worker}`);
      manifest.deployments.push({ component: key, worker: spec.worker, revision, previewId: result.preview_id,
        deploymentId: result.deployment_id, urls, deploymentUrls, images, ...boundaries(config) });
      save();
      const serving = await smokePreview(key, urls[0], { request });
      manifest.deployments.at(-1).serving = serving;
      save();
      if (!serving.passed) fail(`Preview serving check failed for ${spec.worker}; inspect manifest status (no authenticated E2E claimed)`);
      if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY,
        `\n- ${spec.worker}: [${name}](${urls[0]}) — deployment \`${result.deployment_id}\`. Unbridged services and foreign DO bindings target production; see manifest boundaries.\n`);
      console.log(`Preview deployed: ${spec.worker} ${urls[0]}`);
    } finally {
      if (entry) rmSync(entry, { force: true });
      rmSync(generated, { force: true });
      rmSync(receipt, { force: true });
    }
  }
  if (env.GITHUB_OUTPUT) {
    appendFileSync(env.GITHUB_OUTPUT, `preview-manifest=${manifestPath}\n`);
    const accountUrl = manifest.deployments.find(item => item.component === 'account')?.urls[0];
    if (accountUrl) appendFileSync(env.GITHUB_OUTPUT, `url=${accountUrl}\npreview-url=${accountUrl}\n`);
  }
  return manifest;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
