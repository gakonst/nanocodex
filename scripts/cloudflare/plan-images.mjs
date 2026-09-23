#!/usr/bin/env node
// A cache miss requires publication; a matching immutable receipt needs no builder.
import { appendFileSync, existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fingerprint, images, validateReceipt } from './managed-images.mjs';

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

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const plan = planImages({ account: process.env.CLOUDFLARE_ACCOUNT_ID, epoch: process.env.MANAGED_IMAGE_CACHE_EPOCH || '1' });
  appendFileSync(process.env.GITHUB_OUTPUT, `build=${plan.build}\nmatrix=${JSON.stringify(plan.matrix)}\n`);
  console.log(plan.build ? `Images requiring publication: ${plan.matrix.image.join(', ')}` : 'Reusing both immutable images; no builders needed');
}
