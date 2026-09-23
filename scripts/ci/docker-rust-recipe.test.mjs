import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const version = '0.1.78';
const images = [
  { file: 'js/managed/Dockerfile', stage: 'hand', args: '--release --locked -p nanocodex2-bin --bin nanocodex2' },
  { file: 'js/phone-cloud/Dockerfile', stage: 'voice', args: '--release --locked -p nanocodex-phone' },
];

for (const { file, stage, args } of images) {
  test(`${file} exports dependency objects before copying application sources`, () => {
    const dockerfile = readFileSync(join(root, file), 'utf8');
    assert.ok(dockerfile.includes(`RUN cargo install cargo-chef --version ${version} --locked --jobs 2`));
    assert.ok(dockerfile.indexOf('cargo install cargo-chef') < dockerfile.indexOf('COPY '));
    const planner = dockerfile.split(`FROM ${stage}-chef AS ${stage}-planner\n`)[1].split('\nFROM ')[0];
    const builder = dockerfile.split(`FROM ${stage}-chef AS ${stage}\n`)[1].split('\nFROM ')[0];
    assert.ok(planner.includes('RUN cargo chef prepare --recipe-path recipe.json'));
    assert.ok(!planner.includes('cargo chef cook'));
    const cook = `RUN CARGO_BUILD_JOBS=2 cargo chef cook ${args} --recipe-path recipe.json`;
    assert.ok(builder.includes(cook));
    assert.equal(builder.slice(0, builder.indexOf(cook)).split('\n').filter(line => line.startsWith('COPY ')).join('\n'),
      `COPY --from=${stage}-planner /source/recipe.json recipe.json`);
    const sourceCopies = block => block.split('\n').filter(line => line.startsWith('COPY ') && !line.startsWith('COPY --from='));
    assert.deepEqual(sourceCopies(builder), sourceCopies(planner));
    assert.ok(builder.indexOf(sourceCopies(builder)[0]) > builder.indexOf(cook));
    assert.ok(builder.includes(`RUN CARGO_BUILD_JOBS=2 cargo build ${args}`));
    assert.ok(!builder.includes('--mount=type=cache'), 'target, registry and git must survive remote layer export');
  });
}

// Opt in to the real workspace checks with an explicitly installed pinned tool.
// No production compilation: cook --no-build runs only in new empty directories.
const chef = process.env.CARGO_CHEF_TEST_BIN;
test('real Docker contexts have stable recipes and reconstruct their external target paths', { skip: !chef }, async t => {
  const binary = resolve(chef);
  const env = { ...process.env, RUSTUP_TOOLCHAIN: '1.97.0', CARGO_NET_OFFLINE: 'true' };
  const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, env, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  assert.equal(run(binary, ['--version'], root).trim(), `cargo-chef ${version}`);
  assert.match(run('rustc', ['--version'], root), /^rustc 1\.97\.0 /);
  mkdirSync(join(root, '.cache'), { recursive: true });
  const temporary = mkdtempSync(join(root, '.cache/docker-rust-recipe-'));
  try {
    // Exercise the actual sandbox context generator, including tracked assets.
    run(process.execPath, ['js/managed/scripts/prepare-hand-image.mjs'], root);
    const managed = join(temporary, 'managed');
    cpSync(join(root, 'js/managed/.generated/remote-rust'), managed, { recursive: true });
    const phone = join(temporary, 'phone');
    const roots = ['Cargo.toml', 'Cargo.lock', 'bin', 'crates', 'examples', 'js/nanocodex', 'py/bindings', 'third_party'];
    for (const path of run('git', ['ls-files', '-z', '--', ...roots], root).split('\0').filter(Boolean)) {
      mkdirSync(dirname(join(phone, path)), { recursive: true });
      cpSync(join(root, path), join(phone, path));
    }
    const prepare = cwd => {
      run(binary, ['chef', 'prepare', '--recipe-path', 'recipe.json'], cwd);
      return readFileSync(join(cwd, 'recipe.json'), 'utf8');
    };
    const baseline = prepare(managed);
    assert.equal(prepare(phone), baseline, 'both Dockerfiles plan the same tracked Rust workspace');
    const recipe = JSON.parse(baseline);
    assert.equal(recipe.skeleton.config_file, null, 'preserve the existing Docker contexts, which omit .cargo');
    for (const [directory, entrypoint] of [
      [managed, 'bin/nanocodex/src/nanocodex2/main.rs'],
      [phone, 'examples/phone_voice.rs'],
    ]) {
      await t.test(`${directory === managed ? 'sandbox' : 'phone'} invalidation boundaries`, () => {
        const source = join(directory, entrypoint);
        const original = readFileSync(source, 'utf8');
        writeFileSync(source, `${original}\n// Recipe cache source-edit probe.\n`);
        assert.equal(prepare(directory), baseline, 'editing an existing Rust source preserves the dependency layer');
        writeFileSync(source, original);
        const asset = join(directory, 'crates/nanocodex-tools/src/code_mode/bootstrap.js');
        const assetOriginal = readFileSync(asset, 'utf8');
        writeFileSync(asset, `${assetOriginal}\n// Embedded-asset probe.\n`);
        assert.equal(prepare(directory), baseline, 'embedded assets belong to the final source layer');
        writeFileSync(asset, assetOriginal);
        const manifest = join(directory, 'Cargo.toml');
        const manifestOriginal = readFileSync(manifest, 'utf8');
        assert.ok(manifestOriginal.includes('anyhow = "1"'));
        writeFileSync(manifest, manifestOriginal.replace('anyhow = "1"', 'anyhow = "1.0"'));
        assert.notEqual(prepare(directory), baseline, 'dependency manifest changes invalidate the recipe');
        writeFileSync(manifest, manifestOriginal.replace('lto = "thin"', 'lto = "fat"'));
        assert.notEqual(prepare(directory), baseline, 'release profile changes invalidate the recipe');
        writeFileSync(manifest, manifestOriginal);
        const lock = join(directory, 'Cargo.lock');
        const lockOriginal = readFileSync(lock, 'utf8');
        const lockChanged = lockOriginal.replace(/checksum = "([0-9a-f])/, (_, digit) => `checksum = "${digit === '0' ? '1' : '0'}`);
        assert.notEqual(lockChanged, lockOriginal);
        writeFileSync(lock, lockChanged);
        assert.notEqual(prepare(directory), baseline, 'registry lockfile changes invalidate the recipe');
        writeFileSync(lock, lockOriginal);
        // A newly discovered build script must invalidate even without a manifest edit.
        const buildScript = join(directory, 'crates/nanocodex-phone/build.rs');
        assert.ok(!existsSync(buildScript));
        writeFileSync(buildScript, 'fn main() {}\n');
        assert.notEqual(prepare(directory), baseline, 'target discovery captures new build scripts');
        rmSync(buildScript);
        assert.equal(prepare(directory), baseline, 'restoring inputs restores the identical recipe');
      });
    }
    const skeleton = join(temporary, 'skeleton');
    mkdirSync(skeleton);
    assert.deepEqual(readdirSync(skeleton), []);
    writeFileSync(join(skeleton, 'recipe.json'), baseline);
    run(binary, ['chef', 'cook', '--no-build', '--recipe-path', 'recipe.json'], skeleton);
    const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--locked', '--offline', '--format-version', '1'], skeleton));
    for (const [name, path] of [['nanocodex2', 'bin/nanocodex/src/nanocodex2/main.rs'], ['phone-voice-cloud', 'examples/phone_voice.rs']]) {
      const target = metadata.packages.flatMap(pkg => pkg.targets).find(target => target.name === name);
      assert.ok(target, `missing target ${name}`);
      assert.equal(resolve(target.src_path), join(skeleton, path));
      assert.equal(readFileSync(target.src_path, 'utf8'), 'fn main() {}');
    }
    // Overlay real sources exactly as Docker COPY does; entrypoints and embedded
    // assets must replace/augment the dummies before application compilation.
    for (const path of ['bin/nanocodex/src/nanocodex2/main.rs', 'examples/phone_voice.rs', 'crates/nanocodex-tools/src/code_mode/bootstrap.js', 'third_party/codex-voice/webrtc-host/build.rs']) {
      assert.ok(existsSync(join(managed, path)));
    }
    cpSync(managed, skeleton, { recursive: true });
    assert.equal(readFileSync(join(skeleton, 'examples/phone_voice.rs'), 'utf8'), readFileSync(join(root, 'examples/phone_voice.rs'), 'utf8'));
    assert.equal(readFileSync(join(skeleton, 'crates/nanocodex-tools/src/code_mode/bootstrap.js'), 'utf8'), readFileSync(join(root, 'crates/nanocodex-tools/src/code_mode/bootstrap.js'), 'utf8'));
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
});
