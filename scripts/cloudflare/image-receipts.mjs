#!/usr/bin/env node
// Actions caches are accelerators, not the only record of a published image.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ghRequest } from './deployment-ledger.mjs';
import { fingerprint, validateReceipt } from './managed-images.mjs';

export function imageReceiptStore({ repository = process.env.GITHUB_REPOSITORY, ref = process.env.GITHUB_SHA, request = ghRequest } = {}) {
  assert.match(repository, /^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/);
  const base = `repos/${repository}/deployments`;
  const environment = image => {
    assert.ok(['phone', 'sandbox'].includes(image));
    return `nanocodex-image-${image}`;
  };
  return {
    async restore(image, account, input) {
      try {
        // Bound old-history work. A receipt outside this window can rebuild;
        // the most recently used image survives Actions cache eviction.
        const entries = await request({ method: 'GET', path: `${base}?environment=${environment(image)}&per_page=100` });
        assert.ok(Array.isArray(entries));
        for (const entry of entries) {
          if (entry.environment !== environment(image)) continue;
          let payload = entry.payload;
          if (typeof payload === 'string') payload = JSON.parse(payload);
          if (payload?.schema !== 1 || payload.receipt?.input !== input) continue;
          validateReceipt(payload.receipt, image, account, input);
          const statuses = await request({ method: 'GET', path: `${base}/${entry.id}/statuses?per_page=1` });
          if (statuses?.[0]?.state === 'success' && statuses[0].environment === environment(image)) return payload.receipt;
        }
      } catch { /* Unknown or unavailable durable receipt: rebuild normally. */ }
      return null;
    },
    async retain(receipt, account, input) {
      validateReceipt(receipt, receipt.image, account, input);
      const existing = await this.restore(receipt.image, account, input);
      if (existing?.ref !== receipt.ref) await this.save(receipt, account, input);
    },
    async save(receipt, account, input) {
      validateReceipt(receipt, receipt.image, account, input);
      assert.match(ref, /^[a-f0-9]{40}$/);
      const env = environment(receipt.image);
      const entry = await request({ method: 'POST', path: base, body: {
        ref, environment: env, auto_merge: false, required_contexts: [], production_environment: false,
        payload: { schema: 1, receipt }, description: 'Published immutable container image',
      } });
      assert.ok(Number.isSafeInteger(entry?.id) && entry.id > 0);
      assert.equal(entry.environment, env); assert.equal(entry.sha, ref);
      const status = await request({ method: 'POST', path: `${base}/${entry.id}/statuses`, body: {
        state: 'success', environment: env, auto_inactive: false, description: 'Registry digest verified after publication',
      } });
      assert.equal(status?.state, 'success'); assert.equal(status.environment, env);
    },
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const image = process.argv[2], account = process.env.CLOUDFLARE_ACCOUNT_ID;
  const input = fingerprint(image, account, process.env.MANAGED_IMAGE_CACHE_EPOCH || '1');
  const receipt = JSON.parse(readFileSync(`.ci-images/${image}.json`, 'utf8'));
  await imageReceiptStore().retain(receipt, account, input);
}
