#!/usr/bin/env node
import {readFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {planImages} from './plan-images.mjs';
import {fingerprint,main as imageCommand} from './managed-images.mjs';
import {imageReceiptStore} from './image-receipts.mjs';

// Normally images come from preflight. A manual live deploy between preflight
// and the production lock can make managed newly necessary: recover, don't fail
// or let an unrelated early image decision prevent the desired release.
export async function recoverImages({account=process.env.CLOUDFLARE_ACCOUNT_ID,epoch=process.env.MANAGED_IMAGE_CACHE_EPOCH || '1',cwd=process.cwd(),store=imageReceiptStore(),publish=image=>imageCommand(['publish',image]),configure=()=>imageCommand(['config'])}={}) {
  const plan=planImages({account,epoch,cwd});
  for(const image of plan.matrix.image) await publish(image);
  for(const image of ['phone','sandbox']) {
    const receipt=JSON.parse(readFileSync(resolve(cwd,`.ci-images/${image}.json`),'utf8'));
    await store.retain(receipt,account,fingerprint(image,account,epoch,cwd));
  }
  configure();
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) await recoverImages();
