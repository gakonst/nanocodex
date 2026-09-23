#!/usr/bin/env node
// One relay image serves every account controller. Receipts survive cache
// eviction in GitHub Deployments; callers serialize ensure under production CI.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, lstatSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { imageReceiptStore } from './image-receipts.mjs';

const image = 'account-relay';
const context = 'js/account/container';
const dockerfile = `${context}/Dockerfile`;
const helper = 'scripts/cloudflare/account-relay-image.mjs';
const ignoreFiles = [`${context}/.dockerignore`, `${dockerfile}.dockerignore`];
const receiptPath = '.ci-images/account-relay.json';
const configPath = 'js/account/dist/nanocodex/wrangler.json';
const outputPath = 'js/account/dist/nanocodex/wrangler.ci.json';
const git = (cwd, args) => execFileSync('git', args, { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
const repository = account => `registry.cloudflare.com/${account}/nanocodex-ci-${image}`;
const scope = (account, epoch) => {
  assert.match(account, /^[a-f0-9]{32}$/, 'Cloudflare account ID');
  assert.ok(typeof epoch === 'string' && epoch.length > 0, 'image epoch is required');
};

export function imageInputs(cwd = process.cwd()) {
  const records = git(cwd, ['ls-tree', '-rz', 'HEAD', '--', context, helper]).toString().split('\0').filter(Boolean);
  const tracked = new Map(records.map(record => {
    const [metadata, path] = record.split('\t');
    const [mode, type, object] = metadata.split(' ');
    return [path, { mode, type, object, record }];
  }));
  const requireFile = path => {
    const entry = tracked.get(path);
    assert.ok(entry?.type === 'blob' && /^100(644|755)$/.test(entry.mode), `image input must be a committed regular file: ${path}`);
    return entry;
  };
  const docker = git(cwd, ['cat-file', 'blob', requireFile(dockerfile).object]).toString();
  const inputs = new Set([dockerfile, helper, ...ignoreFiles.filter(path => tracked.has(path))]);
  let copies = 0, bases = 0;
  // This image deliberately has no package installation or generated inputs.
  // Fail closed on new build semantics until their input closure is audited.
  for (const source of docker.split(/\r?\n/)) {
    const line = source.trim();
    if (!line || (line.startsWith('#') && !/^#\s*(syntax|escape)\s*=/i.test(line))) continue;
    const match = /^(FROM|WORKDIR|COPY|ENV|EXPOSE|USER|CMD|LABEL)\s+(.+)$/.exec(line);
    assert.ok(match && !line.endsWith('\\'), `unsupported account relay Dockerfile instruction: ${line}`);
    if (match[1] === 'FROM') {
      bases++;
      assert.match(match[2], /^[a-z0-9][a-z0-9./:_@-]*$/, 'relay requires a single literal base image');
    }
    if (match[1] !== 'COPY') continue;
    const parts = match[2].startsWith('[') ? JSON.parse(match[2]) : match[2].split(/\s+/);
    assert.ok(Array.isArray(parts) && parts.length >= 2 && parts.every(part => typeof part === 'string'));
    for (const part of parts.slice(0, -1)) {
      assert.ok(/^(?:\.\/)?[a-zA-Z0-9_][a-zA-Z0-9_./-]*$/.test(part)
        && !part.split('/').includes('..'), `unsupported relay COPY input: ${part}`);
      const path = posix.join(context, part);
      requireFile(path); // directories, globs, stages, URLs and symlinks need an audit
      inputs.add(path);
      copies++;
    }
  }
  assert.equal(bases, 1, 'relay requires one build stage');
  assert.ok(copies > 0, 'relay requires committed COPY inputs');
  return [...inputs].sort().map(path => ({ path, ...requireFile(path) }));
}

export function fingerprint(account, epoch = '1', cwd = process.cwd()) {
  scope(account, epoch);
  const inputs = imageInputs(cwd);
  return createHash('sha256').update(JSON.stringify({ version: 1, image, account, epoch, platform: 'linux/amd64' }))
    .update(inputs.map(entry => entry.record).join('\0')).digest('hex');
}

function assertClean(cwd) {
  const inputs = imageInputs(cwd);
  const dirty = git(cwd, ['status', '--porcelain', '--untracked-files=all', '--ignored', '--',
    ...inputs.map(entry => entry.path), ...ignoreFiles]).toString().trim();
  assert.equal(dirty, '', 'Commit relevant account relay image inputs before reuse or publication');
  // Also reject assume-unchanged/skip-worktree entries and symlink replacements.
  for (const entry of inputs) {
    const path = resolve(cwd, entry.path);
    assert.ok(lstatSync(path).isFile(), `image input is not a regular file: ${entry.path}`);
    assert.ok(readFileSync(path).equals(git(cwd, ['cat-file', 'blob', entry.object])), `dirty account relay image input: ${entry.path}`);
  }
}

export function validateReceipt(receipt, name, account, input) {
  scope(account, '1');
  assert.equal(name, image);
  assert.equal(receipt?.version, 1);
  assert.equal(receipt.image, image);
  assert.match(input, /^[a-f0-9]{64}$/);
  assert.equal(receipt.input, input, 'receipt does not match current account relay inputs');
  const prefix = `${repository(account)}@`;
  assert.ok(typeof receipt.ref === 'string' && receipt.ref.startsWith(prefix), 'receipt uses unexpected account/repository');
  assert.match(receipt.ref.slice(prefix.length), /^sha256:[a-f0-9]{64}$/, 'immutable image digest required');
  return receipt.ref;
}

export const receiptStore = options => imageReceiptStore({ ...options, validate: validateReceipt, allowedImages: [image] });
const writeReceipt = (cwd, receipt) => {
  const path = resolve(cwd, receiptPath);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(receipt, null, 2) + '\n');
};

export async function restore({ account, epoch = '1', cwd = process.cwd(), store = receiptStore() }) {
  const input = fingerprint(account, epoch, cwd);
  assertClean(cwd);
  const path = resolve(cwd, receiptPath);
  if (existsSync(path)) {
    const receipt = JSON.parse(readFileSync(path, 'utf8'));
    validateReceipt(receipt, image, account, input);
    return receipt;
  }
  const receipt = await store.restore(image, account, input);
  if (receipt) {
    validateReceipt(receipt, image, account, input);
    writeReceipt(cwd, receipt);
  }
  return receipt;
}

const execute = (command, args, options) => execFileSync(command, args, { stdio: 'inherit', ...options });
export async function ensure({ account, epoch = '1', cwd = process.cwd(), store = receiptStore(), run = execute,
  testsEnabled = process.env.CI_TESTS_ENABLED !== 'false' }) {
  const input = fingerprint(account, epoch, cwd);
  let receipt = await restore({ account, epoch, cwd, store });
  if (receipt) {
    await store.retain(receipt, account, input);
    return receipt;
  }
  const tag = `nanocodex-ci-${image}:input-${input}`;
  run('docker', ['build', '--platform', 'linux/amd64', '--pull', '-t', tag, '-f', dockerfile, context], { cwd });
  if (testsEnabled) run('docker', ['run', '--rm', '--network', 'none', '--entrypoint', 'node', tag, '--check', '/app/relay.mjs'], { cwd });
  // Wrangler owns registry authentication. No credentials pass through here.
  run('pnpm', ['--filter', 'nanocodex-web', 'exec', 'wrangler', 'containers', 'push', tag], { cwd });
  const tagged = `${repository(account)}:input-${input}`;
  const digests = JSON.parse(run('docker', ['image', 'inspect', tagged, '--format', '{{json .RepoDigests}}'], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }));
  assert.ok(digests === null || Array.isArray(digests), 'Docker RepoDigests must be an array');
  const matches = (digests ?? []).filter(value => typeof value === 'string' && value.startsWith(`${repository(account)}@`));
  let ref;
  if (matches.length) {
    assert.equal(matches.length, 1, 'expected one pushed repository digest');
    [ref] = matches;
  } else {
    const manifest = JSON.parse(run('docker', ['manifest', 'inspect', '-v', tagged], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }));
    assert.match(manifest.Descriptor?.digest ?? '', /^sha256:[a-f0-9]{64}$/, 'registry manifest digest required');
    ref = `${repository(account)}@${manifest.Descriptor.digest}`;
  }
  receipt = { version: 1, image, input, ref };
  validateReceipt(receipt, image, account, input);
  // Source movement during a build must not certify an image for another HEAD.
  assert.equal(fingerprint(account, epoch, cwd), input, 'account relay inputs changed during publication');
  assertClean(cwd);
  await store.save(receipt, account, input);
  writeReceipt(cwd, receipt);
  return receipt;
}

export function deploymentConfig(source, ref, { cwd = process.cwd(), path = resolve(cwd, configPath) } = {}) {
  const result = JSON.parse(source);
  assert.ok(Array.isArray(result.containers) && result.containers.length > 0, 'missing account relay containers');
  const expected = resolve(cwd, dockerfile), expectedContext = resolve(cwd, context);
  for (const container of result.containers) {
    assert.ok(typeof container.image === 'string' && resolve(dirname(path), container.image) === expected,
      `unexpected account container image: ${container.image}`);
    assert.ok(!container.configuration, 'unsupported legacy account container configuration');
    assert.ok(container.image_build_context === undefined
      || resolve(dirname(path), container.image_build_context) === expectedContext, 'unexpected account relay build context');
    assert.ok(container.image_vars === undefined || (container.image_vars !== null
      && typeof container.image_vars === 'object' && !Array.isArray(container.image_vars)
      && Object.keys(container.image_vars).length === 0), 'unsupported account relay build arguments');
    container.image = ref;
    delete container.image_build_context;
    delete container.image_vars;
  }
  // Nested environment containers could silently override the rewritten images.
  for (const environment of Object.values(result.env ?? {})) assert.ok(environment.containers === undefined, 'unexpected environment container override');
  return JSON.stringify(result, null, 2) + '\n';
}

export function configure({ account, epoch = '1', cwd = process.cwd() }) {
  const input = fingerprint(account, epoch, cwd);
  assertClean(cwd);
  const receipt = JSON.parse(readFileSync(resolve(cwd, receiptPath), 'utf8'));
  const ref = validateReceipt(receipt, image, account, input);
  const output = deploymentConfig(readFileSync(resolve(cwd, configPath), 'utf8'), ref, { cwd });
  // Stay beside the Vite config: relative asset/module/migration paths survive.
  writeFileSync(resolve(cwd, outputPath), output);
  return outputPath;
}

export async function main([command], { env = process.env, cwd = process.cwd() } = {}) {
  const options = { account: env.CLOUDFLARE_ACCOUNT_ID, epoch: env.MANAGED_IMAGE_CACHE_EPOCH || '1', cwd };
  if (command === 'key') {
    const input = fingerprint(options.account, options.epoch, cwd);
    console.log(input);
    if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, `input=${input}\n`);
  } else if (command === 'restore' || command === 'check') {
    const receipt = await restore(options);
    if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, `build=${!receipt}\n`);
    console.log(receipt ? 'Reusing immutable account relay image' : 'Account relay image publication required');
  } else if (command === 'ensure') {
    const receipt = await ensure({ ...options, testsEnabled: env.CI_TESTS_ENABLED !== 'false' });
    console.log(`Account relay image: ${receipt.ref}`);
  } else {
    assert.equal(command, 'config', 'expected key, restore, check, ensure, or config');
    console.log(configure(options));
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main(process.argv.slice(2));
