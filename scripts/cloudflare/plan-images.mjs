#!/usr/bin/env node
// A cache miss requires publication; a matching immutable receipt needs no builder.
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fingerprint, images, validateReceipt } from './managed-images.mjs';
import { imageReceiptStore } from './image-receipts.mjs';

export function planImages({ account, epoch = '1', cwd = process.cwd(), receiptDirectory = resolve(cwd, '.ci-images') }) {
  const missing = [];
  for (const image of Object.keys(images)) {
    const path = resolve(receiptDirectory, `${image}.json`);
    if (!existsSync(path)) {
      missing.push(image);
      continue;
    }
    validateReceipt(JSON.parse(readFileSync(path, 'utf8')), image, account, fingerprint(image, account, epoch, cwd));
  }
  return { build: missing.length > 0, matrix: { image: missing } };
}

export async function restoreMissingImages({ account, epoch = '1', cwd = process.cwd(), store = imageReceiptStore() }) {
  await Promise.all(Object.keys(images).map(async image => {
    const path = resolve(cwd, `.ci-images/${image}.json`);
    if (existsSync(path)) return;
    const input = fingerprint(image, account, epoch, cwd);
    const receipt = await store.restore(image, account, input);
    if (!receipt) return;
    validateReceipt(receipt, image, account, input);
    mkdirSync(resolve(cwd, '.ci-images'), { recursive: true });
    writeFileSync(path, JSON.stringify(receipt) + '\n');
    console.log(`Recovered published ${image} receipt from durable release history`);
  }));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await restoreMissingImages({ account: process.env.CLOUDFLARE_ACCOUNT_ID, epoch: process.env.MANAGED_IMAGE_CACHE_EPOCH || '1' });
  const plan = planImages({ account: process.env.CLOUDFLARE_ACCOUNT_ID, epoch: process.env.MANAGED_IMAGE_CACHE_EPOCH || '1' });
  appendFileSync(process.env.GITHUB_OUTPUT, `build=${plan.build}\nmatrix=${JSON.stringify(plan.matrix)}\nphone=${plan.matrix.image.includes('phone')}\nsandbox=${plan.matrix.image.includes('sandbox')}\n`);
  console.log(plan.build ? `Images requiring publication: ${plan.matrix.image.join(', ')}` : 'Reusing both immutable images; no builders needed');
}
