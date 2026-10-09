// Run with: node bin/nanocodex/tests/update_source_e2e.mjs target/debug/nanocodex
// Git, Cargo and native build tools are real; GitHub/PR metadata are local and brew is unavailable.
// Current sources build the nanocodex CLI (package nanocodex-bin) and the
// nanocodex-hand daemon (package nanocodex-hand-daemon); the Hand is installed as
// nanocodex2. Earlier one-package splits and the historical nanocodex2-bin pair
// still build.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdtempSync, mkdirSync, readFileSync, readlinkSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const binary = resolve(process.argv[2] ?? 'target/debug/nanocodex');
const fixture = mkdtempSync(join(tmpdir(), 'nanocodex-source-e2e-'));
const output = resolve('output/update-source-e2e');
mkdirSync(output, { recursive: true });
const transcript = [];
const crossInit = process.platform === 'darwin' && process.arch === 'arm64';
const init = join(output, 'krun-init-blob-fixture/out/init');

function run(program, args, options = {}) {
  const started = performance.now();
  const result = spawnSync(program, args, {
    cwd: options.cwd ?? fixture,
    env: { ...process.env, ...options.env },
    encoding: 'utf8',
    timeout: 300_000,
  });
  const elapsedMs = Math.round(performance.now() - started);
  transcript.push(`$ ${program} ${args.join(' ')}\nexit: ${result.status}\nelapsed: ${elapsedMs} ms\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}\n`);
  if (options.success !== false) {
    assert.equal(result.status, 0, `${program} ${args.join(' ')}: ${result.stderr}`);
  }
  return { ...result, elapsedMs };
}

try {
  const source = join(fixture, 'source');
  const remote = join(fixture, 'remote.git');
  const store = join(fixture, 'install');
  const home = join(fixture, 'home');
  const tools = join(fixture, 'tools');
  const buildLog = join(fixture, 'builds.log');
  mkdirSync(source);
  mkdirSync(store);
  mkdirSync(home);
  mkdirSync(tools);
  writeFileSync(join(store, 'automatic-updates-disabled'), '');
  run('git', ['init', '--bare', remote]);
  run('git', ['init', '-b', 'topic'], { cwd: source });
  const workspace = members => `[workspace]\nresolver = "2"\nmembers = [${members.map(m => `"${m}"`).join(', ')}]\n[profile.nightly]\ninherits = "release"\nlto = false\n`;
  writeFileSync(join(source, 'Cargo.toml'), workspace(['cli', 'hand', 'shared']));
  const buildScript = () => `
fn main() {
    use std::io::Write;
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=STABLE_GIT_COMMIT");
    let mut log = std::fs::OpenOptions::new().create(true).append(true)
        .open(std::env::var("FIXTURE_BUILD_LOG").unwrap()).unwrap();
    writeln!(log, "{} {}", std::env::var("CARGO_PKG_NAME").unwrap(), std::env::var("STABLE_GIT_COMMIT").unwrap()).unwrap();
    ${crossInit ? `
    // Match libkrun's nested Cargo build of a static guest init.
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target = out.join("init-target");
    let status = std::process::Command::new(std::env::var_os("CARGO").unwrap())
        .args(["build", "--locked", "--offline", "--release", "--target", "aarch64-unknown-linux-musl",
            "--manifest-path", "../guest/Cargo.toml", "--target-dir"])
        .arg(&target).env_remove("CARGO_ENCODED_RUSTFLAGS").status().unwrap();
    assert!(status.success(), "static guest build failed");
    std::fs::copy(target.join("aarch64-unknown-linux-musl/release/guest-init"),
        std::env::var_os("FIXTURE_INIT_PATH").unwrap()).unwrap();
    ` : ''}
}
`;
  mkdirSync(join(source, 'shared/src'), { recursive: true });
  writeFileSync(join(source, 'shared/Cargo.toml'), '[package]\nname = "shared"\nversion = "0.1.0"\nedition = "2024"\n[features]\ncli = []\nhand = []\n');
  writeFileSync(join(source, 'shared/build.rs'), buildScript());
  writeFileSync(join(source, 'shared/src/lib.rs'), 'pub fn features() -> (bool, bool) { (cfg!(feature = "cli"), cfg!(feature = "hand")) }\n');
  if (crossInit) {
    for (const file of ['.cargo/config.toml', 'scripts/aarch64-unknown-linux-musl-linker', 'scripts/aarch64-unknown-linux-musl-ar']) {
      mkdirSync(join(source, file, '..'), { recursive: true });
      copyFileSync(new URL(`../../../${file}`, import.meta.url), join(source, file));
    }
    mkdirSync(join(source, 'guest/src'), { recursive: true });
    mkdirSync(join(init, '..'), { recursive: true });
    writeFileSync(join(source, 'guest/Cargo.toml'), '[workspace]\n[package]\nname = "guest-init"\nversion = "0.1.0"\nedition = "2024"\n');
    writeFileSync(join(source, 'guest/build.rs'), `fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let object = out.join("guest.o");
    let archive = out.join("libguest.a");
    assert!(std::process::Command::new(std::env::var_os("CC_aarch64_unknown_linux_musl").unwrap())
        .args(["-c", "guest.c", "-o"]).arg(&object).status().unwrap().success());
    assert!(std::process::Command::new(std::env::var_os("AR_aarch64_unknown_linux_musl").unwrap())
        .arg("crs").arg(&archive).arg(&object).status().unwrap().success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=guest");
}
`);
    writeFileSync(join(source, 'guest/guest.c'), 'int guest_value(void) { return 42; }\n');
    writeFileSync(join(source, 'guest/src/main.rs'), 'unsafe extern "C" { fn guest_value() -> i32; }\nfn main() { println!("{}", unsafe { guest_value() }); }\n');
    run('cargo', ['generate-lockfile', '--offline', '--manifest-path', 'guest/Cargo.toml'], { cwd: source });
  }
  // features: the shared crate features this package enables. The unified
  // package enables both roles; a historical pair enabled one role each.
  // extraBins: further [[bin]] targets sharing src/main.rs (the Hand daemon).
  const writePackage = (dir, packageName, binaryName, features, extraBins = []) => {
    const path = join(source, dir);
    mkdirSync(join(path, 'src'), { recursive: true });
    writeFileSync(join(path, 'Cargo.toml'), `[package]\nname = "${packageName}"\nversion = "0.1.0"\nedition = "2024"\n${[binaryName, ...extraBins].map(name => `[[bin]]\nname = "${name}"\npath = "src/main.rs"\n`).join('')}[features]\ntempo = []\n[dependencies]\nshared = { path = "../shared", features = [${features.map(f => `"${f}"`).join(', ')}] }\n`);
    writeFileSync(join(path, 'src/main.rs'), `fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        // Like the shipped entry points: the source updater supplies the commit.
        println!("{} Version: 0.1.0-dev\\nCommit SHA: {}", env!("CARGO_BIN_NAME"), option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"));
        println!("Shared features: {:?}", shared::features());
    } else if args.get(1).map(String::as_str) == Some("__device-hand") {
        println!("{{\\"serviceProtocol\\":1}}");
    }
}
`);
  };
  writePackage('cli', 'nanocodex-bin', 'nanocodex', ['cli']);
  writePackage('hand', 'nanocodex-hand-daemon', 'nanocodex-hand', ['hand']);
  writeFileSync(join(source, 'nanocodex-vm.entitlements'), '<?xml version="1.0"?><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>');
  run('cargo', ['generate-lockfile', '--offline'], { cwd: source });
  run('git', ['add', '.'], { cwd: source });
  run('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'synthetic source'], { cwd: source });
  const sha = run('git', ['rev-parse', 'HEAD'], { cwd: source }).stdout.trim();
  run('git', ['push', remote, 'HEAD:refs/heads/topic', 'HEAD:refs/pull/42/head'], { cwd: source });

  const gh = join(tools, 'gh');
  writeFileSync(gh, '#!/bin/sh\nprintf \'{"headRefOid":"%s","state":"%s"}\\n\' "$FIXTURE_PR_SHA" "$FIXTURE_PR_STATE"\n', { mode: 0o755 });
  if (crossInit) {
    // Fail even on hosts with brew installed: the build must use installed tools directly.
    writeFileSync(join(tools, 'brew'), '#!/bin/sh\necho "brew is unavailable in this fixture" >&2\nexit 1\n', { mode: 0o755 });
  }
  const env = {
    HOME: home,
    CARGO_HOME: process.env.CARGO_HOME ?? join(process.env.HOME, '.cargo'),
    RUSTUP_HOME: process.env.RUSTUP_HOME ?? join(process.env.HOME, '.rustup'),
    NANOCODEX_DIR: store,
    PATH: `${tools}:${process.env.PATH}`,
    GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: `url.${remote}.insteadOf`,
    GIT_CONFIG_VALUE_0: 'https://github.com/gakonst/nanocodex.git',
    FIXTURE_PR_SHA: sha,
    FIXTURE_PR_STATE: 'OPEN',
    FIXTURE_BUILD_LOG: buildLog,
    FIXTURE_INIT_PATH: init,
  };
  const update = (args, changes = {}) => run(binary, ['update', ...args], { env: { ...env, ...changes }, success: false });

  let result = update(['--branch', 'topic']);
  assert.equal(result.status, 0, result.stderr);
  const firstBuildMs = result.elapsedMs;
  const splitPair = key => {
    const cliVersion = run(join(store, 'versions', key, 'nanocodex'), ['--version']).stdout;
    const handVersion = run(join(store, 'versions', key, 'nanocodex2'), ['--version']).stdout;
    assert.match(cliVersion, /^nanocodex Version/m);
    assert.match(handVersion, /^nanocodex-hand Version/m, 'the Hand is installed under the service name nanocodex2');
    transcript.push(`observed: versions/${key}/nanocodex is the CLI, nanocodex2 is the nanocodex-hand build`);
  };

  splitPair(`branch-${sha}`);
  assert.match(result.stderr, /compiling nanocodex and nanocodex-hand at /);
  assert.doesNotMatch(result.stderr, /nanocodex2-bin/);
  const installedVersion = run(join(store, 'versions', `branch-${sha}`, 'nanocodex'), ['--version']).stdout;
  assert.match(installedVersion, new RegExp(sha));
  assert.match(installedVersion, /Shared features: \(true, true\)/);
  if (crossInit) {
    assert.equal(readFileSync(init).readUInt16LE(18), 183, 'guest init must be AArch64');
    run('python3', [fileURLToPath(new URL('../../../scripts/check-vm-init.py', import.meta.url)), output]);
    transcript.push('expected: with brew unavailable, nested Cargo builds C, archives it and links a static AArch64 musl init using shipped wrappers; observed: static ELF verified');
  }
  const firstBuild = readFileSync(buildLog, 'utf8');
  assert.equal(firstBuild.trim().split('\n').length, 2, firstBuild);

  result = update(['--branch', 'topic']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(buildLog, 'utf8'), firstBuild, 'unchanged branch must reuse both binaries and shared dependencies');
  transcript.push(`expected: one shared dependency build, one package build for both binaries; repeated update compiles nothing\nobserved: first update ${firstBuildMs} ms; repeated update ${result.elapsedMs} ms\nobserved build log:\n${firstBuild}`);

  result = update(['--pr', '42']);
  assert.equal(result.status, 0, result.stderr);
  assert.ok(existsSync(join(store, 'versions', `pr-42-${sha}`, 'nanocodex2')));
  assert.match(run(join(store, 'versions', `pr-42-${sha}`, 'nanocodex2'), ['--version']).stdout, new RegExp(sha));
  assert.equal(readFileSync(buildLog, 'utf8'), firstBuild, 'PR at the same revision must reuse the branch build');

  result = update(['--pr', '42'], { FIXTURE_PR_STATE: 'CLOSED' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /refusing to build a stale head/);
  result = update(['--pr', '42'], { FIXTURE_PR_SHA: '0'.repeat(40) });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /head changed while fetching/);
  result = update(['--branch', 'missing']);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /failed to fetch Nanocodex source/);

  // A newer upstream revision must replace cached executables and identity.
  writeFileSync(join(source, 'shared/src/lib.rs'), 'pub fn features() -> (bool, bool) { (cfg!(feature = "cli"), cfg!(feature = "hand")) }\npub fn revision() -> u8 { 2 }\n');
  run('git', ['add', '.'], { cwd: source });
  run('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'next revision'], { cwd: source });
  const nextSha = run('git', ['rev-parse', 'HEAD'], { cwd: source }).stdout.trim();
  run('git', ['push', remote, 'HEAD:refs/heads/topic'], { cwd: source });
  result = update(['--branch', 'topic']);
  assert.equal(result.status, 0, result.stderr);
  for (const executable of ['nanocodex', 'nanocodex2']) {
    assert.match(run(join(store, 'versions', `branch-${nextSha}`, executable), ['--version']).stdout, new RegExp(nextSha));
  }
  assert.equal(readFileSync(buildLog, 'utf8').trim().split('\n').length, 4);
  splitPair(`branch-${nextSha}`);

  // Recover a modified cached checkout instead of activating altered sources.
  writeFileSync(join(store, 'source-build/checkout/cli/src/main.rs'), 'invalid cached Rust\n');
  result = update(['--branch', 'topic']);
  assert.equal(result.status, 0, result.stderr);
  assert.match(run(join(store, 'versions', `branch-${nextSha}`, 'nanocodex'), ['--version']).stdout, new RegExp(nextSha));

  writeFileSync(join(source, 'cli/src/main.rs'), 'this is invalid Rust\n');
  run('git', ['add', '.'], { cwd: source });
  run('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'broken build'], { cwd: source });
  run('git', ['push', remote, 'HEAD:refs/heads/broken'], { cwd: source });
  result = update(['--branch', 'broken']);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /cargo failed while compiling nanocodex/);
  assert.ok(existsSync(join(store, 'versions', `branch-${sha}`, 'nanocodex2')));
  const active = existsSync(join(store, 'pending-update'))
    ? readFileSync(join(store, 'pending-update'), 'utf8').trim()
    : readlinkSync(join(store, 'current')).split('/').at(-1);
  assert.equal(active, `branch-${nextSha}`);
  transcript.push(`expected: branch and PR binaries built at ${sha}, new revision ${nextSha} installed, modified cache recovered; closed, changed, missing and broken heads rejected; previous bundle preserved\nobserved: ${active}\n`);

  // An earlier split revision built both executables from one nanocodex-bin
  // package; it still installs a revision-matched pair.
  writeFileSync(join(source, 'Cargo.toml'), workspace(['cli', 'shared']));
  rmSync(join(source, 'hand'), { recursive: true, force: true });
  writePackage('cli', 'nanocodex-bin', 'nanocodex', ['cli', 'hand'], ['nanocodex-hand']);
  run('cargo', ['generate-lockfile', '--offline'], { cwd: source });
  run('git', ['add', '-A', '.'], { cwd: source });
  run('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'one-package split'], { cwd: source });
  const onePackageSha = run('git', ['rev-parse', 'HEAD'], { cwd: source }).stdout.trim();
  run('git', ['push', remote, 'HEAD:refs/heads/one-package-split'], { cwd: source });
  result = update(['--branch', 'one-package-split']);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /compiling nanocodex and nanocodex-hand at /);
  splitPair(`branch-${onePackageSha}`);
  transcript.push(`expected: one-package split revision ${onePackageSha} installs its pair
observed: nanocodex and nanocodex2 at ${onePackageSha}
`);

  // A historical revision with the separate nanocodex2-bin package keeps its
  // distinct, revision-matched pair (update --branch of an old topic branch).
  writeFileSync(join(source, 'Cargo.toml'), workspace(['cli', 'hand', 'shared']));
  writePackage('cli', 'nanocodex-bin', 'nanocodex', ['cli']);
  writePackage('hand', 'nanocodex2-bin', 'nanocodex2', ['hand']);
  run('cargo', ['generate-lockfile', '--offline'], { cwd: source });
  run('git', ['add', '.'], { cwd: source });
  run('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'historical pair'], { cwd: source });
  const legacySha = run('git', ['rev-parse', 'HEAD'], { cwd: source }).stdout.trim();
  run('git', ['push', remote, 'HEAD:refs/heads/legacy-pair'], { cwd: source });
  result = update(['--branch', 'legacy-pair']);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /compiling nanocodex and nanocodex2 at /);
  const legacyCli = run(join(store, 'versions', `branch-${legacySha}`, 'nanocodex'), ['--version']).stdout;
  const legacyHand = run(join(store, 'versions', `branch-${legacySha}`, 'nanocodex2'), ['--version']).stdout;
  assert.match(legacyCli, /^nanocodex Version/m);
  assert.match(legacyCli, /Shared features: \(true, true\)/);
  assert.match(legacyHand, /^nanocodex2 Version/m);
  assert.match(legacyHand, /Shared features: \(true, true\)/);
  for (const output of [legacyCli, legacyHand]) assert.match(output, new RegExp(legacySha));
  transcript.push(`expected: historical two-package revision ${legacySha} builds and installs its distinct pair\nobserved: nanocodex and nanocodex2 report their own packages at ${legacySha}\n`);
  process.stdout.write(`source update journeys passed; transcript: ${join(output, 'transcript.log')}\n`);
} finally {
  writeFileSync(join(output, 'transcript.log'), transcript.join('\n'));
  rmSync(fixture, { recursive: true, force: true });
}
