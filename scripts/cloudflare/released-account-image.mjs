#!/usr/bin/env node
// API releases consume published images. This module never builds or publishes.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deploymentConfig, validateReceipt } from './account-relay-image.mjs';
import { ghRequest } from './deployment-ledger.mjs';
import { accountValid } from './live-worker-state.mjs';

const image = 'account-relay';
const environment = 'nanocodex-image-account-relay';
const configPath = 'js/account/dist/nanocodex/wrangler.json';
const outputPath = 'js/account/dist/nanocodex/wrangler.ci.json';
const failure = () => new Error('Released account relay image could not be resolved; no image build was attempted');

export async function lastReleasedAccountReceipt({ account, repository = process.env.GITHUB_REPOSITORY, request = ghRequest } = {}) {
  try {
    assert.ok(accountValid(account));
    assert.match(repository, /^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/);
    const base = `repos/${repository}/deployments`;
    const entries = await request({ method: 'GET', path: `${base}?environment=${environment}&per_page=100` });
    assert.ok(Array.isArray(entries));
    for (const entry of entries) {
      if (entry.environment !== environment || !Number.isSafeInteger(entry.id) || entry.id <= 0) continue;
      let receipt;
      try {
        const payload = typeof entry.payload === 'string' ? JSON.parse(entry.payload) : entry.payload;
        assert.equal(payload?.schema, 1);
        receipt = payload.receipt;
        // The API release intentionally does not fingerprint current image inputs.
        validateReceipt(receipt, image, account, receipt?.input);
      } catch { continue; }
      const statuses = await request({ method: 'GET', path: `${base}/${entry.id}/statuses?per_page=1` });
      if (statuses?.[0]?.state === 'success' && statuses[0].environment === environment) return receipt;
    }
  } catch { /* Bootstrap from live applications if publication history is unavailable. */ }
  return null;
}


async function pinnedReceipt({ account, cwd, repository, receiptRequest }) {
  assert.ok(accountValid(account));
  const path = resolve(cwd, '.ci-images/released-account.json');
  const readPin = () => {
    const pin = JSON.parse(readFileSync(path, 'utf8'));
    assert.equal(pin.version, 1);
    assert.equal(pin.account, account);
    assert.equal(pin.repository, repository ?? null);
    assert.ok(Object.hasOwn(pin, 'receipt'));
    if (pin.receipt !== null) validateReceipt(pin.receipt, image, account, pin.receipt?.input);
    return pin.receipt;
  };
  if (existsSync(path)) return readPin();
  const receipt = await lastReleasedAccountReceipt({ account, repository, request: receiptRequest });
  mkdirSync(dirname(path), { recursive: true });
  try {
    // First writer wins if planning/configuration happen to overlap. Null pins
    // bootstrap too, so a newly published image cannot change this job's plan.
    writeFileSync(path, `${JSON.stringify({ version: 1, account, repository: repository ?? null, receipt })}\n`, { flag: 'wx' });
  } catch (error) { if (error.code !== 'EEXIST') throw error; }
  return readPin();
}

export async function releasedAccountIdentity({
  account = process.env.CLOUDFLARE_ACCOUNT_ID, cwd = process.cwd(),
  repository = process.env.GITHUB_REPOSITORY, receiptRequest = ghRequest,
} = {}) {
  try {
    const receipt = await pinnedReceipt({ account, cwd, repository, receiptRequest });
    return receipt?.ref ?? 'live';
  } catch { throw failure(); }
}

export function applicationName(config, container) {
  if (container.name !== undefined) {
    assert.ok(typeof container.name === 'string' && container.name.length > 0);
    return container.name;
  }
  assert.ok(typeof config.name === 'string' && config.name.length > 0);
  assert.ok(typeof container.class_name === 'string' && container.class_name.length > 0);
  // Wrangler 4.127.1 default, when no named environment is selected. Vite's
  // generated config has already resolved any explicit container name.
  return `${config.name}-${container.class_name}`.toLowerCase().replace(/ /g, '-');
}

function deployedImage(value, account) {
  // Preserve provider image URIs, never local paths or untagged build inputs.
  assert.ok(typeof value === 'string');
  const match = /^([a-z0-9]+(?:[._-][a-z0-9]+)*(?:\/[a-z0-9]+(?:[._-][a-z0-9]+)*)*)(?::([A-Za-z0-9_][A-Za-z0-9._-]{0,127}))?(?:@(sha256:[a-f0-9]{64}))?$/.exec(value);
  assert.ok(match && (match[2] || match[3]) && match[2] !== 'latest');
  const prefix = `registry.cloudflare.com/${account}/`;
  if (value.startsWith('registry.cloudflare.com/')) {
    const suffix = value.slice('registry.cloudflare.com/'.length);
    const namespace = /^([a-f0-9]{32})\//.exec(suffix);
    assert.ok(!namespace || namespace[1] === account);
    return namespace ? value : `${prefix}${suffix}`;
  }
  // Wrangler's resolveImageName qualifies short names with this account.
  return match[1].split('/')[0].includes('.') ? value : `${prefix}${value}`;
}

export async function deployedAccountImages(config, {
  account, token = process.env.CLOUDFLARE_API_TOKEN, request = globalThis.fetch,
} = {}) {
  try {
    assert.ok(accountValid(account) && typeof token === 'string' && token.length > 0);
    // Wrangler 4.127.1 ApplicationsService.listApplications() uses the v4
    // envelope at this exact endpoint; no Worker version/bindings API is needed.
    const response = await request(`https://api.cloudflare.com/client/v4/accounts/${account}/containers/applications`, {
      method: 'GET', redirect: 'error', signal: AbortSignal.timeout(30_000),
      headers: { Authorization: `Bearer ${token}`, 'Cache-Control': 'no-cache' },
    });
    if (!response.ok) throw failure();
    const body = await response.json();
    assert.equal(body?.success, true);
    assert.ok(Array.isArray(body.result));
    const names = config.containers.map(container => applicationName(config, container));
    assert.equal(new Set(names).size, names.length);
    return names.map(name => {
      const matches = body.result.filter(app => app.name === name);
      assert.equal(matches.length, 1);
      const app = matches[0];
      assert.ok(typeof app.durable_objects?.namespace_id === 'string' && app.durable_objects.namespace_id.length > 0);
      return deployedImage(app.configuration?.image, account);
    });
  } catch {
    // Never propagate tokens, provider bodies, private application config or causes.
    throw failure();
  }
}

export async function configureReleasedAccount({
  account = process.env.CLOUDFLARE_ACCOUNT_ID, cwd = process.cwd(),
  repository = process.env.GITHUB_REPOSITORY, token = process.env.CLOUDFLARE_API_TOKEN,
  receiptRequest = ghRequest, request = globalThis.fetch,
} = {}) {
  try {
    assert.ok(accountValid(account));
    const source = readFileSync(resolve(cwd, configPath), 'utf8');
    const sourceConfig = JSON.parse(source);
    assert.ok(Array.isArray(sourceConfig.containers));
    assert.ok(sourceConfig.containers.every(container => container.unsafe === undefined));
    // Reuse the established config validation and sibling-path behavior.
    const config = JSON.parse(deploymentConfig(source, 'pending-released-image', { cwd }));
    const receipt = await pinnedReceipt({ account, cwd, repository, receiptRequest });
    if (receipt) {
      const ref = validateReceipt(receipt, image, account, receipt.input);
      for (const container of config.containers) container.image = ref;
    } else {
      const refs = await deployedAccountImages(config, { account, token, request });
      config.containers.forEach((container, index) => { container.image = refs[index]; });
    }
    writeFileSync(resolve(cwd, outputPath), `${JSON.stringify(config, null, 2)}\n`);
    return outputPath;
  } catch { throw failure(); }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(await configureReleasedAccount());
}
