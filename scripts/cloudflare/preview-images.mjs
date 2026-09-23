#!/usr/bin/env node
// Compare the same committed image keys used by production, not broad JS paths.
import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fingerprint } from './managed-images.mjs';

// Only compare source identities here. Both revisions use the same synthetic
// account so preview selection needs no deployment credentials.
export function changedImages({ base, account = '0'.repeat(32), epoch = '1', cwd = process.cwd() }) {
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
export function previewPlan(options = {}) {
  const changed = options.event === 'workflow_dispatch'
    ? ['phone', 'sandbox'] : changedImages(options);
  return {
    changed,
    required: changed.length > 0,
    rollout: changed.length ? 'immediate' : 'none',
    // GitHub validates matrices even for skipped jobs; keep the empty plan valid.
    matrix: { image: changed.length ? changed : ['phone', 'sandbox'] },
  };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const plan = previewPlan({
    base: process.env.BASE_SHA,
    event: process.env.GITHUB_EVENT_NAME,
    epoch: process.env.MANAGED_IMAGE_CACHE_EPOCH || '1',
  });
  appendFileSync(process.env.GITHUB_OUTPUT,
    `rollout=${plan.rollout}\nrequired=${plan.required}\nmatrix=${JSON.stringify(plan.matrix)}\n`);
  console.log(plan.required ? `Changed image inputs: ${plan.changed.join(', ')}` : 'No managed image input changes');
}
