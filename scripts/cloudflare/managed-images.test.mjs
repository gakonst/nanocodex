import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { imageInputs, fingerprint, validateReceipt, registryDigest, deploymentConfig } from './managed-images.mjs';

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
    put('Cargo.toml', '[workspace]');
    put('crates/nanocodex-phone/Cargo.toml', '[package]\nname = "nanocodex-phone"');
    put('crates/nanocodex-remote/Cargo.toml', '[package]\nname = "nanocodex2-bin"'); put('js/managed/Dockerfile', 'FROM scratch');
    put('hands/remote/image/labwc/config', 'desktop'); commit();
    const firstPhone = fingerprint('phone', account, '1', dir);
    const firstSandbox = fingerprint('sandbox', account, '1', dir);
    put('js/managed/src/index.ts', 'Worker-only change'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), firstPhone);
    assert.equal(fingerprint('sandbox', account, '1', dir), firstSandbox);
    for (const path of ['js/nanocodex/package.json', 'examples/unrelated.rs', 'bin/nanousd/src/lib.rs', 'hands/remote/README.md']) put(path, 'unrelated'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), firstPhone);
    assert.equal(fingerprint('sandbox', account, '1', dir), firstSandbox);
    put('js/managed/scripts/phone-bridge.mjs', 'phone change'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), firstPhone);
    assert.equal(fingerprint('sandbox', account, '1', dir), firstSandbox);
    put('hands/remote/image/labwc/config', 'new desktop'); commit();
    assert.notEqual(fingerprint('sandbox', account, '1', dir), firstSandbox);
    const beforeRust = fingerprint('sandbox', account, '1', dir);
    put('crates/nanocodex-remote/src/runtime.rs', 'new shared publisher'); commit();
    assert.notEqual(fingerprint('sandbox', account, '1', dir), beforeRust);
    put('crates/new-local/Cargo.toml', '[package]\nname = "new-local"');
    put('crates/nanocodex-phone/Cargo.toml', '[package]\nname = "nanocodex-phone"\n[dependencies]\nnew-local = { path = "../new-local" }'); commit();
    const beforeDependency = fingerprint('phone', account, '1', dir);
    put('crates/new-local/src/lib.rs', 'new transitive input'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), beforeDependency);
    const current = fingerprint('sandbox', account, '1', dir);
    assert.notEqual(fingerprint('sandbox', account, '2', dir), current);
    assert.notEqual(fingerprint('sandbox', 'f'.repeat(32), '1', dir), current);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('Rust inputs follow local Cargo packages and external binary sources', () => {
  const phone = imageInputs('phone');
  const sandbox = imageInputs('sandbox');
  const covers = (inputs, path) => inputs.some(p => p === path || path.startsWith(p + '/'));
  for (const inputs of [phone, sandbox]) {
    for (const path of ['Cargo.lock', 'crates/nanocodex-managed/src/lib.rs', 'crates/nanocodex-tools/src/code_mode/bootstrap.js']) assert.ok(covers(inputs, path), path);
    for (const path of ['js/managed/src/index.ts', 'js/nanocodex/package.json', 'examples/unrelated.rs', 'bin/nanousd/src/lib.rs']) assert.ok(!covers(inputs, path), path);
  }
  for (const path of ['examples/phone_voice.rs', 'examples/phone_audio.rs', 'examples/phone_capture.rs']) assert.ok(covers(phone, path), path);
  assert.ok(!covers(phone, 'crates/nanocodex-remote/src/lib.rs'));
  for (const path of ['bin/nanocodex/src/nanocodex2/main.rs', 'bin/nanocodex/src/computer.rs', 'bin/nanocodex/src/clipboard.rs', 'hands/remote/image/labwc/rc.xml', 'crates/nanocodex-vm/image/toolkit/python.txt']) assert.ok(covers(sandbox, path), path);
  assert.ok(!covers(sandbox, 'hands/remote/README.md'));
});

test('publication records the pushed digest and never publishes after failed image verification', () => {
  const dir = mkdtempSync(join(tmpdir(), 'managed-image-publish-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
  try {
    mkdirSync(join(dir, 'scripts/cloudflare'), { recursive: true });
    mkdirSync(join(dir, 'js/phone-cloud'), { recursive: true });
    mkdirSync(join(dir, 'commands'));
    for (const file of ['managed-images.mjs', 'managed-image-inputs.py', 'wrangler-docker.mjs']) {
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
    for (const overrides of [{}, { MISSING_LOCAL_DIGEST: '1' }, { FAIL_DOCKER: 'run' }, { FAIL_DOCKER: 'run', CI_TESTS_ENABLED: 'false' }]) {
      rmSync(join(dir, '.ci-images'), { recursive: true, force: true });
      writeFileSync(capture, '');
      const result = spawnSync(process.execPath, ['scripts/cloudflare/managed-images.mjs', 'publish', 'phone'], {
        cwd: dir, encoding: 'utf8', env: { ...process.env, PATH: join(dir, 'commands') + ':' + process.env.PATH,
          CAPTURE: capture, GITHUB_WORKSPACE: dir, BUILDX_BUILDER: 'test-builder', CLOUDFLARE_ACCOUNT_ID: account,
          MANAGED_IMAGE_CACHE_EPOCH: '1', CI_TESTS_ENABLED: 'true', ...overrides },
      });
      const commands = readFileSync(capture, 'utf8').trim().split('\n').map(line => JSON.parse(line));
      const build = commands.find(command => command.includes('buildx'));
      assert.ok(build.includes('--pull') && build.includes('NANOCODEX_IMAGE_CACHE_EPOCH=1'));
      if (overrides.FAIL_DOCKER && overrides.CI_TESTS_ENABLED !== 'false') {
        assert.notEqual(result.status, 0);
        assert.ok(!commands.some(command => command[0] === 'pnpm'), 'must not publish an unverified image');
      } else {
        assert.equal(result.status, 0, result.stderr);
        const receipt = JSON.parse(readFileSync(join(dir, '.ci-images/phone.json'), 'utf8'));
        assert.equal(validateReceipt(receipt, 'phone', account, fingerprint('phone', account, '1', dir)), ref('phone'));
        assert.ok(commands.some(command => command.includes('push')));
        assert.equal(commands.some(command => command[0] === 'docker' && command[1] === 'run'), overrides.CI_TESTS_ENABLED !== 'false');
        assert.ok(!commands.some(command => command.includes('login')), 'Wrangler owns credential handling');
      }
    }
    mkdirSync(join(dir, 'crates/new/src'), { recursive: true });
    writeFileSync(join(dir, 'crates/new/src/lib.rs'), 'uncommitted build input');
    writeFileSync(capture, '');
    const dirty = spawnSync(process.execPath, ['scripts/cloudflare/managed-images.mjs', 'publish', 'phone'], {
      cwd: dir, encoding: 'utf8', env: { ...process.env, PATH: join(dir, 'commands') + ':' + process.env.PATH,
        CAPTURE: capture, GITHUB_WORKSPACE: dir, BUILDX_BUILDER: 'test-builder', CLOUDFLARE_ACCOUNT_ID: account },
    });
    assert.notEqual(dirty.status, 0);
    assert.match(dirty.stderr, /Commit relevant image inputs/);
    assert.equal(readFileSync(capture, 'utf8'), '');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('Linux image closure excludes proven target mismatches and retains unknown optional and host dependencies', () => {
  const fixture = rustInputFixture();
  const { dir, put, commit, covers } = fixture;
  try {
    const cases = [
      ['windows', 'cfg(windows)', false],
      ['wasm', 'cfg(target_family = "wasm")', false],
      ['macos', 'cfg(not(target_os = "linux"))', false],
      ['other-triple', 'aarch64-unknown-linux-gnu', false],
      ['linux', 'cfg(all(target_arch = "x86_64", target_env = "gnu"))', true],
      ['linux-triple', 'x86_64-unknown-linux-gnu', true],
      ['unix', 'cfg (unix)', true],
      ['unknown', 'cfg(unknown_flag)', true],
      ['any-unknown', 'cfg(any(windows, unknown_flag))', true],
      ['all-unknown', 'cfg(all(windows, unknown_flag))', false],
      ['unknown-feature', 'cfg(feature = "optional-feature")', true],
      ['future-cfg', 'cfg(future_predicate(target_os = "linux"))', true],
    ];
    let manifest = '[package]\nname = "nanocodex-phone"\n[dependencies]\nalways = { path = "../always", optional = true }\nmacro = { path = "../macro" }\n';
    for (const [name, platform] of cases) {
      put(`crates/${name}/Cargo.toml`, `[package]\nname = "${name}"\n`);
      put(`crates/${name}/src/lib.rs`, `source for ${name}`);
      manifest += `[target.'${platform}'.dependencies]\n${name} = { path = "../${name}", optional = true }\n`;
    }
    manifest += '[target.\'cfg(unix)\'.build-dependencies]\nbuild-linux = { path = "../build-linux" }\n[target.\'cfg(windows)\'.build-dependencies]\nbuild-windows = { path = "../build-windows" }\n';
    put('crates/nanocodex-phone/Cargo.toml', manifest);
    for (const name of ['always', 'build-linux', 'build-windows', 'host-linux', 'host-windows']) {
      put(`crates/${name}/Cargo.toml`, `[package]\nname = "${name}"\n`);
      put(`crates/${name}/src/lib.rs`, `source for ${name}`);
    }
    put('crates/macro/Cargo.toml', '[package]\nname = "macro"\n[lib]\nproc-macro = true\n[target.\'cfg(unix)\'.dependencies]\nhost-linux = { path = "../host-linux" }\n[target.\'cfg(windows)\'.dependencies]\nhost-windows = { path = "../host-windows" }\n');
    put('crates/macro/src/lib.rs', 'proc macro source');
    commit();
    const inputs = imageInputs('phone', dir);
    for (const [name, , included] of cases) assert.equal(covers(inputs, `crates/${name}/src/lib.rs`), included, name);
    for (const name of ['always', 'build-linux', 'host-linux']) assert.ok(covers(inputs, `crates/${name}/src/lib.rs`), name);
    for (const name of ['build-windows', 'host-windows']) assert.ok(!covers(inputs, `crates/${name}/src/lib.rs`), name);
    const original = fingerprint('phone', account, '1', dir);
    put('crates/windows/src/lib.rs', 'native-only change'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), original);
    put('crates/linux/src/lib.rs', 'production Linux change'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), original);
  } finally { fixture.close(); }
});

test('image closure omits standalone tests but preserves build scripts production targets and includes', () => {
  const fixture = rustInputFixture();
  const { dir, put, commit, covers } = fixture;
  try {
    let manifest = '[package]\nname = "nanocodex-phone"\n[dependencies]\n';
    for (const name of ['plain', 'build', 'custom', 'explicit', 'included']) {
      manifest += `${name} = { path = "../${name}" }\n`;
      put(`crates/${name}/Cargo.toml`, `[package]\nname = "${name}"\n${name === 'custom' ? 'build = "generate.rs"\n' : ''}${name === 'explicit' ? '[lib]\npath = "tests/library.rs"\n' : ''}`);
      put(`crates/${name}/src/lib.rs`, `source for ${name}`);
      put(`crates/${name}/tests/standalone.rs`, 'integration test');
      put(`crates/${name}/benches/standalone.rs`, 'benchmark');
    }
    put('crates/nanocodex-phone/Cargo.toml', manifest);
    put('crates/build/build.rs', 'fn main() {}');
    put('crates/custom/generate.rs', 'fn main() {}');
    put('crates/explicit/tests/library.rs', 'mod child;');
    put('crates/explicit/tests/child.rs', 'production child');
    put('crates/included/src/lib.rs', 'const PROMPT: &str = include_str!("../tests/prompt.txt");\nconst DATA: &[u8] = include_bytes!(r#"../benches/data.bin"#);\n#[path = "../tests/runtime.rs"] mod runtime;');
    put('crates/included/tests/prompt.txt', 'embedded prompt');
    put('crates/included/benches/data.bin', 'embedded data');
    put('crates/included/tests/runtime.rs', 'mod sibling;');
    put('crates/included/tests/sibling.rs', 'production sibling');
    commit();
    const inputs = imageInputs('phone', dir);
    for (const name of ['plain', 'included']) {
      for (const kind of ['tests', 'benches']) assert.ok(!covers(inputs, `crates/${name}/${kind}/standalone.rs`), `${name}/${kind}`);
    }
    for (const name of ['build', 'custom']) {
      for (const kind of ['tests', 'benches']) assert.ok(covers(inputs, `crates/${name}/${kind}/standalone.rs`), `${name}/${kind}`);
    }
    for (const path of ['crates/explicit/tests/library.rs', 'crates/explicit/tests/child.rs', 'crates/included/tests/prompt.txt', 'crates/included/benches/data.bin', 'crates/included/tests/runtime.rs', 'crates/included/tests/sibling.rs']) assert.ok(covers(inputs, path), path);
    const original = fingerprint('phone', account, '1', dir);
    put('crates/plain/tests/standalone.rs', 'changed standalone test'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), original);
    put('crates/included/tests/prompt.txt', 'changed production prompt'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), original);
  } finally { fixture.close(); }
});

test('excluded voice workspace manifests enter image keys only through the selected Cargo closure', () => {
  const fixture = rustInputFixture();
  const { dir, put, commit, covers } = fixture;
  try {
    const voiceRoot = 'third_party/codex-voice';
    put('Cargo.toml', '[workspace]\nexclude = ["third_party/codex-voice"]\n');
    put(`${voiceRoot}/Cargo.toml`, '[workspace]\n[workspace.dependencies]\nvoice-core = { path = "core" }\n');
    put(`${voiceRoot}/voice/Cargo.toml`, '[package]\nname = "voice"\n[dependencies]\nvoice-core.workspace = true\n');
    put(`${voiceRoot}/voice/src/lib.rs`, 'voice source');
    put(`${voiceRoot}/core/Cargo.toml`, '[package]\nname = "voice-core"\n');
    put(`${voiceRoot}/core/src/lib.rs`, 'voice core source');
    put(`${voiceRoot}/unused/Cargo.toml`, 'excluded invalid TOML must not be parsed');
    commit();
    const original = fingerprint('phone', account, '1', dir);
    assert.ok(!imageInputs('phone', dir).some(path => path.startsWith(voiceRoot + '/')));
    put(`${voiceRoot}/Cargo.toml`, '[workspace]\n[workspace.dependencies]\nvoice-core = { path = "core" }\n# excluded change\n'); commit();
    assert.equal(fingerprint('phone', account, '1', dir), original);
    put('crates/nanocodex-phone/Cargo.toml', '[package]\nname = "nanocodex-phone"\n[dependencies]\nvoice = { path = "../../third_party/codex-voice/voice" }\n'); commit();
    const inputs = imageInputs('phone', dir);
    for (const path of [`${voiceRoot}/Cargo.toml`, `${voiceRoot}/voice/Cargo.toml`, `${voiceRoot}/core/Cargo.toml`, `${voiceRoot}/core/src/lib.rs`]) assert.ok(covers(inputs, path), path);
    assert.ok(!covers(inputs, `${voiceRoot}/unused/Cargo.toml`));
    const activated = fingerprint('phone', account, '1', dir);
    put(`${voiceRoot}/core/src/lib.rs`, 'changed selected voice core'); commit();
    assert.notEqual(fingerprint('phone', account, '1', dir), activated);
  } finally { fixture.close(); }
});

function rustInputFixture() {
  const dir = mkdtempSync(join(tmpdir(), 'managed-linux-inputs-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
  const put = (path, content) => { mkdirSync(dirname(join(dir, path)), { recursive: true }); writeFileSync(join(dir, path), content); };
  git('init', '-q');
  put('Cargo.toml', '[workspace]\n');
  put('crates/nanocodex-phone/Cargo.toml', '[package]\nname = "nanocodex-phone"\n');
  put('crates/nanocodex-phone/src/lib.rs', 'phone source');
  return {
    dir, put,
    commit: () => { git('add', '.'); git('-c', 'user.name=CI', '-c', 'user.email=ci@example.invalid', 'commit', '-qm', 'fixture'); },
    covers: (inputs, path) => inputs.some(input => input === path || path.startsWith(input + '/')),
    close: () => rmSync(dir, { recursive: true, force: true }),
  };
}
