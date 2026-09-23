#!/usr/bin/env node
// API releases use published images without waiting for a new native build.
// Freeze image selection for the job so later publications cannot move its plan.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ghRequest } from './deployment-ledger.mjs';
import { deploymentConfig, fingerprint as currentFingerprint, validateReceipt } from './managed-images.mjs';

const names = ['phone', 'sandbox'];
function validateImages(images, account) {
  assert.ok(images && typeof images === 'object' && !Array.isArray(images));
  assert.deepEqual(Object.keys(images).sort(), [...names].sort());
  for (const image of names) validateReceipt(images[image], image, account, images[image]?.input);
  return images;
}
function readPinned(path, account) {
  const pinned = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(pinned.schema, 1);
  assert.equal(pinned.account, account, 'released image selection belongs to another account');
  return validateImages(pinned.images, account);
}

export async function resolveReleasedImages({ account = process.env.CLOUDFLARE_ACCOUNT_ID,
  cwd = process.cwd(), repository = process.env.GITHUB_REPOSITORY, request = ghRequest,
  epoch = process.env.MANAGED_IMAGE_CACHE_EPOCH || '1', fingerprint = currentFingerprint, requireCurrent = false } = {}) {
  assert.match(account, /^[a-f0-9]{32}$/, 'Cloudflare account ID');
  const path = resolve(cwd, '.ci-images/released.json');
  const desired = () => Object.fromEntries(names.map(image => [image, fingerprint(image, account, epoch, cwd)]));
  let expected;
  let images;
  if (existsSync(path)) images = readPinned(path, account);
  else {
    assert.match(repository, /^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/);
    expected = desired();
    const base = `repos/${repository}/deployments`;
    const released = await Promise.all(names.map(async image => {
      const environment = `nanocodex-image-${image}`;
      const entries = await request({ method: 'GET', path: `${base}?environment=${environment}&per_page=100` });
      assert.ok(Array.isArray(entries), 'invalid published image history');
      // Prefer this checkout's inputs even if a stale publisher finished later.
      // Otherwise API releases may keep the latest successfully published image.
      const candidates = [];
      for (const entry of entries.slice(0, 100)) {
        if (entry?.environment !== environment || !Number.isSafeInteger(entry.id) || entry.id <= 0) continue;
        let receipt;
        try {
          const payload = typeof entry.payload === 'string' ? JSON.parse(entry.payload) : entry.payload;
          assert.equal(payload?.schema, 1);
          receipt = payload.receipt;
          // Validate its original input, never pretend it represents current HEAD.
          validateReceipt(receipt, image, account, receipt?.input);
        } catch { continue; }
        candidates.push({ entry, receipt });
      }
      const current = candidates.filter(({ receipt }) => receipt.input === expected[image]);
      const older = requireCurrent ? [] : candidates.filter(({ receipt }) => receipt.input !== expected[image]);
      // Avoid fetching statuses for unrelated history before a usable current
      // receipt, or for all 100 entries just to find the latest fallback.
      for (const { entry, receipt } of [...current, ...older]) {
        const statuses = await request({ method: 'GET', path: `${base}/${entry.id}/statuses?per_page=1` });
        if (Array.isArray(statuses) && statuses[0]?.state === 'success' && statuses[0].environment === environment) return receipt;
      }
      if (requireCurrent) throw new Error(`No successful published ${image} image for current inputs; image rollout deferred`);
      throw new Error(`No successful published ${image} image for this account; publish an image before deploying managed`);
    }));
    images = validateImages(Object.fromEntries(names.map((image, index) => [image, released[index]])), account);
    mkdirSync(dirname(path), { recursive: true });
    try {
      writeFileSync(path, JSON.stringify({ schema: 1, account, images }, null, 2) + '\n', { flag: 'wx' });
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      images = readPinned(path, account);
    }
  }
  if (requireCurrent) {
    expected ??= desired();
    for (const image of names) validateReceipt(images[image], image, account, expected[image]);
  }
  const refs = Object.fromEntries(names.map(image => [image, images[image].ref]));
  const source = readFileSync(resolve(cwd, 'js/managed/wrangler.jsonc'), 'utf8');
  writeFileSync(resolve(cwd, 'js/managed/wrangler.ci.jsonc'), deploymentConfig(source, refs));
  return images;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  assert.ok(args.length === 0 || (args.length === 1 && args[0] === '--current'), 'Usage: released-images.mjs [--current]');
  const images = await resolveReleasedImages({ requireCurrent: args.includes('--current') });
  console.log(`Using published phone ${images.phone.ref} and sandbox ${images.sandbox.ref}`);
}
