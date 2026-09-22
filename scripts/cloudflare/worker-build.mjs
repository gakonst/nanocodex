#!/usr/bin/env node
// Transfer only build outputs between jobs in the same workflow run. Source,
// dependencies and Wrangler credentials come from the receiving job's checkout.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const outputs = [
  'js/nanocodex/pkg-web', 'js/nanocodex/pkg-node',
  'js/nanocodex-tools/dist', 'js/nanocodex-terminal/dist',
  'js/nanocodex-connect-protocol/dist', 'js/nanocodex-connect-ui/dist',
  'js/account/dist', 'js/connect-dialog/dist', 'js/connect-playground/dist',
  'examples/astra-mpp-trial/public/client.js',
];
const directory = '.ci-worker-build';
const archive = `${directory}/outputs.tar.gz`;
const manifest = `${directory}/manifest.json`;
const sha256 = data => createHash('sha256').update(data).digest('hex');

function checkFiles(cwd, path) {
  const info = lstatSync(join(cwd, path));
  assert.ok(!info.isSymbolicLink(), `build output must not be a symlink: ${path}`);
  if (info.isDirectory()) {
    const entries = readdirSync(join(cwd, path));
    assert.ok(entries.length, `empty build output: ${path}`);
    for (const entry of entries) checkFiles(cwd, `${path}/${entry}`);
  } else assert.ok(info.isFile(), `build output must be a regular file: ${path}`);
}

export function transfer(command, { cwd = process.cwd(), revision, runId } = {}) {
  assert.match(revision ?? '', /^[a-f0-9]{40}$/, 'build revision must be a commit SHA');
  assert.match(runId ?? '', /^\d+$/, 'build must belong to a workflow run');
  if (command === 'pack') {
    for (const path of outputs) checkFiles(cwd, path);
    mkdirSync(join(cwd, directory), { recursive: true });
    execFileSync('tar', ['-czf', archive, ...outputs], { cwd });
    writeFileSync(join(cwd, manifest), JSON.stringify({
      version: 1, revision, runId, outputs,
      sha256: sha256(readFileSync(join(cwd, archive))),
    }) + '\n');
    return;
  }
  assert.equal(command, 'restore');
  const receipt = JSON.parse(readFileSync(join(cwd, manifest), 'utf8'));
  assert.equal(receipt.version, 1);
  assert.equal(receipt.revision, revision, 'Worker build does not match checkout');
  assert.equal(receipt.runId, runId, 'Worker build belongs to another workflow run');
  assert.deepEqual(receipt.outputs, outputs, 'unexpected Worker build output set');
  assert.equal(receipt.sha256, sha256(readFileSync(join(cwd, archive))), 'Worker build archive is corrupt');
  // The archive is produced by the same-run build job, never a cross-run cache.
  execFileSync('tar', ['-xzf', archive], { cwd });
  for (const path of outputs) checkFiles(cwd, path);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const revision = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  assert.equal(revision, process.env.GITHUB_SHA, 'checkout must match workflow revision');
  transfer(process.argv[2], { revision, runId: process.env.GITHUB_RUN_ID });
}
