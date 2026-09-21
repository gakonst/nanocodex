#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Input keys select reusable receipts; deployment always uses the actual registry
// digest. Bump MANAGED_IMAGE_CACHE_EPOCH to refresh mutable upstream base images.
export const images = {
  phone: {
    dockerfile: 'js/phone-cloud/Dockerfile', context: '.', source: '../phone-cloud/Dockerfile',
    inputs: ['.dockerignore', 'js/phone-cloud', 'Cargo.toml', 'Cargo.lock', 'bin', 'crates',
      'examples', 'js/nanocodex', 'py/bindings', 'third_party',
      'js/managed/scripts/phone-bridge.mjs', 'js/managed/scripts/phone-delegation.mjs',
      'js/managed/src/twilio-voice.ts'],
  },
  sandbox: {
    dockerfile: 'js/managed/Dockerfile', context: 'js/managed', source: './Dockerfile',
    inputs: ['js/managed/Dockerfile', 'js/managed/Dockerfile.dockerignore', 'js/managed/.dockerignore',
      'js/managed/scripts/prepare-hand-image.mjs', 'js/managed/scripts/bundle-hand-desktop.sh',
      'js/managed/scripts/check-dev-stack.sh', 'hands/remote', 'Cargo.toml', 'Cargo.lock',
      'bin', 'crates', 'examples', 'js/nanocodex', 'py/bindings', 'third_party'],
  },
};
const commonInputs = ['scripts/cloudflare/managed-images.mjs', 'scripts/cloudflare/wrangler-docker.mjs'];
export function fingerprint(image, account, epoch = '1', cwd = process.cwd()) {
  assert.ok(images[image], 'unknown managed image');
  assert.match(account, /^[a-f0-9]{32}$/, 'Cloudflare account ID');
  const tree = execFileSync('git', ['ls-tree', '-rz', 'HEAD', '--', ...images[image].inputs, ...commonInputs], { cwd });
  assert.ok(tree.length, 'image inputs must be committed');
  return createHash('sha256').update(JSON.stringify({ version: 1, image, account, epoch, platform: 'linux/amd64' }))
    .update(tree).digest('hex');
}
export function validateReceipt(receipt, image, account, expectedInput) {
  assert.ok(images[image]);
  assert.equal(receipt.version, 1);
  assert.equal(receipt.image, image);
  assert.equal(receipt.input, expectedInput, 'receipt does not match current image inputs');
  assert.match(expectedInput, /^[a-f0-9]{64}$/);
  assert.match(account, /^[a-f0-9]{32}$/);
  const prefix = `registry.cloudflare.com/${account}/nanocodex-ci-${image}@`;
  assert.ok(receipt.ref.startsWith(prefix), 'receipt uses unexpected account/repository');
  assert.match(receipt.ref.slice(prefix.length), /^sha256:[a-f0-9]{64}$/, 'immutable image digest required');
  return receipt.ref;
}
export function registryDigest(repoDigests, repository) {
  assert.ok(Array.isArray(repoDigests), 'Docker RepoDigests must be an array');
  const matches = repoDigests.filter(ref => ref.startsWith(`${repository}@`) && /^sha256:[a-f0-9]{64}$/.test(ref.slice(repository.length + 1)));
  assert.equal(matches.length, 1, 'expected one digest for the pushed repository');
  return matches[0];
}
export function deploymentConfig(source, refs) {
  // Keep the generated file beside wrangler.jsonc so all module/migration paths
  // retain their original meaning. Fail closed if config grows another image.
  const seen = new Set();
  const output = source.replace(/("image"\s*:\s*)"([^"]+)"/g, (_, prefix, value) => {
    const image = Object.keys(images).find(name => images[name].source === value);
    assert.ok(image && refs[image], `unexpected container image: ${value}`);
    seen.add(image);
    return `${prefix}${JSON.stringify(refs[image])}`;
  }).replace(/\s*"image_build_context"\s*:\s*"[^"]*",?/g, '');
  assert.equal(seen.size, Object.keys(images).length, 'missing expected managed container');
  return output;
}
const run = (command, args, options = {}) => execFileSync(command, args, { stdio: 'inherit', ...options });
const receiptPath = image => `.ci-images/${image}.json`;
export function main([command, image]) {
  const account = process.env.CLOUDFLARE_ACCOUNT_ID;
  const epoch = process.env.MANAGED_IMAGE_CACHE_EPOCH || '1';
  if (command === 'config') {
    const refs = Object.fromEntries(Object.keys(images).map(name => [name, validateReceipt(
      JSON.parse(readFileSync(receiptPath(name), 'utf8')), name, account, fingerprint(name, account, epoch))]));
    writeFileSync('js/managed/wrangler.ci.jsonc', deploymentConfig(readFileSync('js/managed/wrangler.jsonc', 'utf8'), refs));
    return;
  }
  assert.ok(images[image], 'unknown managed image');
  const input = fingerprint(image, account, epoch);
  if (command === 'fingerprint') {
    appendFileSync(process.env.GITHUB_OUTPUT, `input=${input}\n`);
    return;
  }
  if (command === 'verify') {
    validateReceipt(JSON.parse(readFileSync(receiptPath(image), 'utf8')), image, account, input);
    return;
  }
  assert.equal(command, 'publish');
  const spec = images[image];
  const tag = `nanocodex-ci-${image}:input-${input}`;
  const repository = `registry.cloudflare.com/${account}/nanocodex-ci-${image}`;
  if (image === 'sandbox') run(process.execPath, ['js/managed/scripts/prepare-hand-image.mjs']);
  run(process.execPath, ['scripts/cloudflare/wrangler-docker.mjs', 'build', '--load', '-t', tag,
    '--platform', 'linux/amd64', '--provenance=false', '-f', spec.dockerfile, spec.context]);
  if (image === 'sandbox') {
    for (const check of ['nanocodex-check-dev-stack', 'nanocodex-check-hand-toolkit']) {
      run('docker', ['run', '--rm', '--network', 'none', '--entrypoint', 'sh', tag, '-lc', check]);
    }
  } else {
    run('docker', ['run', '--rm', '--network', 'none', '--entrypoint', 'node', tag, '--check', 'scripts/phone-bridge.mjs']);
  }
  // Wrangler handles registry login internally; no credentials are read here.
  run('pnpm', ['--filter', 'nanocodex-managed-service', 'exec', 'wrangler', 'containers', 'push', tag]);
  const tagged = `${repository}:input-${input}`;
  const digests = JSON.parse(execFileSync('docker', ['image', 'inspect', tagged, '--format', '{{json .RepoDigests}}'], { encoding: 'utf8' }));
  // Some Docker versions do not attach RepoDigests to the local image after a
  // push. Use the same authenticated manifest fallback as Wrangler itself.
  let ref;
  assert.ok(digests === null || Array.isArray(digests));
  if ((digests ?? []).some(value => value.startsWith(`${repository}@`))) {
    ref = registryDigest(digests, repository);
  } else {
    const manifest = JSON.parse(execFileSync('docker', ['manifest', 'inspect', '-v', tagged], { encoding: 'utf8' }));
    assert.match(manifest.Descriptor?.digest ?? '', /^sha256:[a-f0-9]{64}$/);
    ref = `${repository}@${manifest.Descriptor.digest}`;
  }
  const receipt = { version: 1, image, input, ref };
  validateReceipt(receipt, image, account, input);
  mkdirSync('.ci-images', { recursive: true });
  writeFileSync(receiptPath(image), JSON.stringify(receipt, null, 2) + '\n');
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2));
