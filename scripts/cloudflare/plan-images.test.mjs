import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { planImages } from './plan-images.mjs';
import { fingerprint } from './managed-images.mjs';

test('only missing immutable image receipts schedule builders; invalid receipts fail closed', () => {
  const receiptDirectory = mkdtempSync(join(tmpdir(), 'image-plan-'));
  const account = 'a'.repeat(32);
  const options = { account, receiptDirectory };
  const receipt = image => ({ version: 1, image, input: fingerprint(image, account), ref: `registry.cloudflare.com/${account}/nanocodex-ci-${image}@sha256:${'b'.repeat(64)}` });
  try {
    assert.deepEqual(planImages(options), {build:true,matrix:{image:['phone','sandbox']}});
    writeFileSync(join(receiptDirectory,'phone.json'), JSON.stringify(receipt('phone')));
    assert.deepEqual(planImages(options), {build:true,matrix:{image:['sandbox']}});
    writeFileSync(join(receiptDirectory,'sandbox.json'), JSON.stringify(receipt('sandbox')));
    assert.deepEqual(planImages(options), {build:false,matrix:{image:[]}});
    writeFileSync(join(receiptDirectory,'phone.json'), JSON.stringify({...receipt('phone'),input:'c'.repeat(64)}));
    assert.throws(()=>planImages(options), /receipt does not match/);
  } finally { rmSync(receiptDirectory,{recursive:true,force:true}); }
});
