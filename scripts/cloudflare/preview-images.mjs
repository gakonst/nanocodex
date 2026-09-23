#!/usr/bin/env node
// Compare the same committed image keys used by production, not broad JS paths.
import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fingerprint } from './managed-images.mjs';

export function changedImages({ base, account, epoch = '1', cwd = process.cwd() }) {
  if (!/^[a-f0-9]{40}$/.test(base ?? '')) return ['phone', 'sandbox'];
  const directory = mkdtempSync(join(tmpdir(), 'nanocodex-image-base-'));
  const checkout = join(directory, 'checkout');
  try {
    execFileSync('git', ['worktree', 'add', '--detach', checkout, base], { cwd, stdio: 'ignore' });
    return ['phone', 'sandbox'].filter(image => fingerprint(image, account, epoch, cwd) !== fingerprint(image, account, epoch, checkout));
  } catch { return ['phone', 'sandbox']; }
  finally {
    try { execFileSync('git', ['worktree', 'remove', '--force', checkout], { cwd, stdio: 'ignore' }); } catch {}
    rmSync(directory, { recursive: true, force: true });
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const changed = changedImages({ base: process.env.BASE_SHA, account: process.env.CLOUDFLARE_ACCOUNT_ID, epoch: process.env.MANAGED_IMAGE_CACHE_EPOCH || '1' });
  const rollout = changed.length ? 'immediate' : 'none';
  appendFileSync(process.env.GITHUB_OUTPUT, `rollout=${rollout}\n`);
  console.log(changed.length ? `Changed image inputs: ${changed.join(', ')}` : 'No managed image input changes');
}
