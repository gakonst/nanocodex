import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { images, fingerprint, validateReceipt, registryDigest, deploymentConfig } from './managed-images.mjs';

const account = 'a'.repeat(32), digest = 'b'.repeat(64), input = 'c'.repeat(64);
const ref = image => `registry.cloudflare.com/${account}/nanocodex-ci-${image}@sha256:${digest}`;
test('receipts accept only current inputs, account, repository, and immutable digest', () => {
  const receipt = { version: 1, image: 'phone', input, ref: ref('phone') };
  assert.equal(validateReceipt(receipt, 'phone', account, input), ref('phone'));
  for (const patch of [{ version: 2 }, { image: 'sandbox' }, { input: digest },
    { ref: ref('sandbox') }, { ref: ref('phone').replace(account, 'd'.repeat(32)) },
    { ref: `registry.cloudflare.com/${account}/nanocodex-ci-phone:latest` },
    { ref: ref('phone') + '/extra' }]) {
    assert.throws(() => validateReceipt({ ...receipt, ...patch }, 'phone', account, input));
  }
  assert.equal(registryDigest(['other/repo@sha256:' + digest, ref('phone')], `registry.cloudflare.com/${account}/nanocodex-ci-phone`), ref('phone'));
  assert.throws(() => registryDigest([], 'wrong-repo'));
});
test('registry config keeps relative module paths and resources, refuses unknown images', () => {
  const source = readFileSync(new URL('../../js/managed/wrangler.jsonc', import.meta.url), 'utf8');
  const result = deploymentConfig(source, { phone: ref('phone'), sandbox: ref('sandbox') });
  assert.ok(result.includes(ref('phone')) && result.includes(ref('sandbox')));
  assert.ok(!result.includes('"image_build_context"'));
  assert.ok(!/"image"\s*:\s*"(?:\.\/Dockerfile|\.\.\/phone-cloud\/Dockerfile)"/.test(result));
  // Comparing the unchanged config text catches inadvertent edits to bindings,
  // migration history, resource sizes, and module paths without parsing JSONC.
  const canonical = value => value.replace(/"image"\s*:\s*"[^"]+"/g, '"image":"IMAGE"')
    .replace(/\s*"image_build_context"\s*:\s*"[^"]*",?/g, '');
  assert.equal(canonical(result), canonical(source));
  assert.throws(() => deploymentConfig(source.replace('../phone-cloud/Dockerfile', './future/Dockerfile'), { phone: ref('phone'), sandbox: ref('sandbox') }));
});
test('input receipts survive unrelated commits and invalidate every relevant source change', () => {
  const dir = mkdtempSync(join(tmpdir(), 'managed-image-inputs-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
  const put = (path, content) => { mkdirSync(dirname(join(dir, path)), { recursive: true }); writeFileSync(join(dir, path), content); };
  const commit = () => { git('add', '.'); git('-c', 'user.name=CI', '-c', 'user.email=ci@example.invalid', 'commit', '-qm', 'fixture'); };
  try {
    git('init', '-q');
    put('Cargo.toml', 'workspace'); put('js/managed/Dockerfile', 'FROM scratch');
    put('hands/remote/image/labwc/config', 'desktop'); commit();
    const firstPhone = fingerprint('phone', account, '1', dir);
    const firstSandbox = fingerprint('sandbox', account, '1', dir);
    put('js/managed/src/index.ts', 'Worker-only change'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), firstPhone);
    assert.equal(fingerprint('sandbox', account, '1', dir), firstSandbox);
    put('js/managed/scripts/phone-bridge.mjs', 'phone change'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), firstPhone);
    assert.equal(fingerprint('sandbox', account, '1', dir), firstSandbox);
    put('hands/remote/image/labwc/config', 'new desktop'); commit();
    assert.notEqual(fingerprint('sandbox', account, '1', dir), firstSandbox);
    const current = fingerprint('sandbox', account, '1', dir);
    assert.notEqual(fingerprint('sandbox', account, '2', dir), current);
    assert.notEqual(fingerprint('sandbox', 'f'.repeat(32), '1', dir), current);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('every Docker COPY input is covered by the receipt fingerprint', () => {
  for (const [image, spec] of Object.entries(images)) {
    const dockerfile = readFileSync(new URL('../../' + spec.dockerfile, import.meta.url), 'utf8');
    for (const line of dockerfile.split('\n')) {
      if (!/^(COPY|ADD) /.test(line) || /--from=/.test(line)) continue;
      assert.ok(!line.includes('[') && !line.endsWith('\\'), 'update input audit for new Dockerfile syntax');
      const sources = line.split(/\s+/).slice(1).filter(token => !token.startsWith('--')).slice(0, -1);
      for (let source of sources) {
        if (source.startsWith('.generated/hand/')) source = 'hands/remote';
        else if (source.startsWith('.generated/toolkit/')) source = 'crates/nanocodex-vm/image/toolkit';
        else source = spec.context === '.' ? source.replace(/^\.\//, '') : spec.context + '/' + source;
        assert.ok(spec.inputs.some(path => source === path || source.startsWith(path + '/')),
          `${image}: ${source} is not fingerprinted`);
      }
    }
  }
});

test('publication records the pushed digest and never publishes after failed image verification', () => {
  const dir = mkdtempSync(join(tmpdir(), 'managed-image-publish-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
  try {
    mkdirSync(join(dir, 'scripts/cloudflare'), { recursive: true });
    mkdirSync(join(dir, 'js/phone-cloud'), { recursive: true });
    mkdirSync(join(dir, 'commands'));
    for (const file of ['managed-images.mjs', 'wrangler-docker.mjs']) {
      copyFileSync(new URL(file, import.meta.url), join(dir, 'scripts/cloudflare', file));
    }
    writeFileSync(join(dir, 'js/phone-cloud/Dockerfile'), 'FROM scratch\n');
    const capture = join(dir, 'commands.jsonl');
    writeFileSync(join(dir, 'commands/docker'), `#!${process.execPath}
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.CAPTURE, JSON.stringify(['docker', ...args])+'\\n');
if (args[0] === process.env.FAIL_DOCKER) process.exit(42);
if (args[0] === 'image') console.log(JSON.stringify(process.env.MISSING_LOCAL_DIGEST ? [] : [${JSON.stringify(ref('phone'))}]));
if (args[0] === 'manifest') console.log(JSON.stringify({Descriptor:{digest:'sha256:${digest}'}}));
`, { mode: 0o755 });
    writeFileSync(join(dir, 'commands/pnpm'), `#!${process.execPath}
require('node:fs').appendFileSync(process.env.CAPTURE, JSON.stringify(['pnpm', ...process.argv.slice(2)])+'\\n');
`, { mode: 0o755 });
    git('init', '-q'); git('add', '.');
    git('-c', 'user.name=CI', '-c', 'user.email=ci@example.invalid', 'commit', '-qm', 'fixture');
    for (const overrides of [{}, { MISSING_LOCAL_DIGEST: '1' }, { FAIL_DOCKER: 'run' }]) {
      rmSync(join(dir, '.ci-images'), { recursive: true, force: true });
      writeFileSync(capture, '');
      const result = spawnSync(process.execPath, ['scripts/cloudflare/managed-images.mjs', 'publish', 'phone'], {
        cwd: dir, encoding: 'utf8', env: { ...process.env, PATH: join(dir, 'commands') + ':' + process.env.PATH,
          CAPTURE: capture, GITHUB_WORKSPACE: dir, BUILDX_BUILDER: 'test-builder', CLOUDFLARE_ACCOUNT_ID: account,
          MANAGED_IMAGE_CACHE_EPOCH: '1', ...overrides },
      });
      const commands = readFileSync(capture, 'utf8').trim().split('\n').map(line => JSON.parse(line));
      if (overrides.FAIL_DOCKER) {
        assert.notEqual(result.status, 0);
        assert.ok(!commands.some(command => command[0] === 'pnpm'), 'must not publish an unverified image');
      } else {
        assert.equal(result.status, 0, result.stderr);
        const receipt = JSON.parse(readFileSync(join(dir, '.ci-images/phone.json'), 'utf8'));
        assert.equal(validateReceipt(receipt, 'phone', account, fingerprint('phone', account, '1', dir)), ref('phone'));
        assert.ok(commands.some(command => command.includes('push')));
        assert.ok(!commands.some(command => command.includes('login')), 'Wrangler owns credential handling');
      }
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
