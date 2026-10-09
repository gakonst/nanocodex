// Public updater boundary test. Requires the real nanocodex CLI and nanocodex-hand
// built from one source revision (cargo build -p nanocodex-bin --bins). The Hand
// is installed under its service file name nanocodex2. Never starts/stops an OS
// service: Darwin uses an unregistered synthetic plist and asserts the live Hand
// PID is unchanged. --old-updater PATH also installs the pair with a previously
// shipped two-binary updater (a read-only copy) before the new CLI takes over.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

assert.ok(['darwin', 'linux'].includes(process.platform),
  'Windows native acceptance needs a disposable interactive Windows user; this runner must not replace a live Windows CLI/task.');
const argv = process.argv.slice(2);
const oldUpdaterIndex = argv.indexOf('--old-updater');
const suppliedOldUpdater = oldUpdaterIndex >= 0 ? resolve(argv[oldUpdaterIndex + 1] ?? '') : null;
if (oldUpdaterIndex >= 0) argv.splice(oldUpdaterIndex, 2);
const withSource = argv.includes('--source');
const positional = argv.filter(arg => arg !== '--source');
assert.ok(positional[0] && positional[1],
  'usage: node bin/nanocodex/tests/update_local_e2e.mjs CLI HAND [OUTPUT_DIR] [--source] [--old-updater PATH]');
const suppliedCli = resolve(positional[0]);
const suppliedHand = resolve(positional[1]);
const output = resolve(positional[2] ?? 'output/update-local-e2e');
mkdirSync(output, { recursive: true });
const fixture = mkdtempSync(join(tmpdir(), 'nanocodex updater & pair '));
const trace = [];
let verdict = 'FAILED';
const store = join(fixture, 'install');
const home = join(fixture, 'home');
const runner = join(fixture, 'runner', 'nanocodex');
const cli = join(fixture, 'pair', 'nanocodex');
// Built layout: nanocodex-hand beside the CLI is found without --hand-binary.
const hand = join(fixture, 'pair', 'nanocodex-hand');
const explicitHand = join(fixture, 'explicit', 'hand-build');
const cliOnly = join(fixture, 'cli-only', 'nanocodex');
const account = join(home, 'synthetic-account.json');
for (const path of [store, home, join(fixture, 'runner'), join(fixture, 'pair'), dirname(explicitHand), dirname(cliOnly)]) {
  mkdirSync(path, { recursive: true });
}
for (const [source, target] of [[suppliedCli, runner], [suppliedCli, cli], [suppliedHand, hand],
  [suppliedHand, explicitHand], [suppliedCli, cliOnly]]) {
  copyFileSync(source, target);
  chmodSync(target, 0o755);
}
writeFileSync(join(store, 'automatic-updates-disabled'), '');
// Deliberately invalid synthetic account: no saved credential is ever read.
writeFileSync(account, JSON.stringify({ fixture: true, access_token: 'synthetic-not-a-real-credential' }), { mode: 0o600 });
const accountBefore = readFileSync(account);
// Drop inherited account, provider, target, and install-path overrides. No GitHub
// access is needed. Keep only command discovery and OS temp variables.
const env = {
  PATH: process.env.PATH, HOME: home, USERPROFILE: home,
  LOCALAPPDATA: join(home, 'localappdata'), XDG_CONFIG_HOME: join(home, '.config'),
  NANOCODEX_DIR: store, NANOCODEX_ACCOUNT_FILE: account, NO_COLOR: '1',
  TMPDIR: process.env.TMPDIR ?? tmpdir(), TMP: tmpdir(), TEMP: tmpdir(),
};
function run(program, args, expected = 0, options = {}) {
  writeFileSync(join(output, 'transcript.log'), `${trace.join('\n\n')}\n\nrunning: ${program} ${args.join(' ')}\n`);
  const r = spawnSync(program, args, { cwd: options.cwd ?? fixture, env: { ...env, ...options.env }, encoding: 'utf8', timeout: options.timeout ?? 1_800_000, maxBuffer: 16 * 1024 * 1024 });
  trace.push(`$ ${program} ${args.join(' ')}\nexpected: ${expected === 0 ? 'success' : 'failure'}\nobserved exit: ${r.status}; signal: ${r.signal}; error: ${r.error?.message ?? 'none'}\nstdout:\n${r.stdout ?? ''}\nstderr:\n${r.stderr ?? ''}`);
  assert.equal(r.error, undefined, 'child execution error');
  if (expected === 0) assert.equal(r.status, 0, r.stderr);
  else assert.notEqual(r.status, 0, 'unexpected success');
  return r;
}
const update = (args, expected = 0) => run(runner, ['update', ...args], expected);
const active = () => basename(readlinkSync(join(store, 'current')));
const pending = () => existsSync(join(store, 'pending-update')) ? readFileSync(join(store, 'pending-update'), 'utf8').trim() : null;
const versions = () => run('find', [join(store, 'versions'), '-maxdepth', '1', '-type', 'd']).stdout.split('\n').filter(Boolean).sort();
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const oneRevision = (stdout, what) => {
  const lines = stdout.match(/^Commit SHA: .*$/gm) ?? [];
  assert.equal(lines.length, 1, `${what} --version must print exactly one Commit SHA line`);
  return lines[0].match(/^Commit SHA: ([0-9a-f]{40})$/i)?.[1].toLowerCase();
};
// Help structure distinguishes the trees: the managed tree takes only a
// command (no top-level options), while only the local tree has `auth`.
const managedHelp = /^Usage: \S+ \[COMMAND\]$/m;
const localHelp = /^\s+auth\s/m;
function checkAliases(root, revision, label) {
  for (const alias of ['nanocodex', 'nanocodex2', 'nc', 'ncl']) {
    const link = join(root, 'bin', alias);
    assert.equal(readlinkSync(link), join('..', 'current', 'nanocodex'), `${label}: bin/${alias} must link the CLI`);
    assert.equal(oneRevision(run(link, ['--version']).stdout, `${label} bin/${alias}`), revision);
  }
  const ncl = run(join(root, 'bin', 'ncl'), ['--help']).stdout;
  assert.match(ncl, localHelp, `${label}: ncl --help must show the local tree`);
  assert.doesNotMatch(ncl, managedHelp, `${label}: ncl --help must not show the managed tree`);
  for (const alias of ['nanocodex', 'nanocodex2', 'nc']) {
    const help = run(join(root, 'bin', alias), ['--help']).stdout;
    assert.match(help, managedHelp, `${label}: ${alias} --help must show the managed tree`);
    assert.doesNotMatch(help, localHelp, `${label}: ${alias} --help must not show the local tree`);
  }
  trace.push(`PASS ${label}: bin/{nanocodex,nanocodex2,nc,ncl} -> ../current/nanocodex; each prints one Commit SHA ${revision}; ncl --help local tree, the others managed tree`);
}
function bundleBytes(directory, label, handBytes = suppliedHand) {
  assert.equal(digest(readFileSync(join(directory, 'nanocodex'))), digest(readFileSync(suppliedCli)), `${label}: CLI bytes`);
  assert.equal(digest(readFileSync(join(directory, 'nanocodex2'))), digest(readFileSync(handBytes)), `${label}: Hand bytes as nanocodex2`);
  trace.push(`PASS ${label}: ${directory}/nanocodex = CLI, nanocodex2 = Hand sha256 ${digest(readFileSync(join(directory, 'nanocodex2')))}`);
}
function handPid() {
  if (process.platform !== 'darwin') return null;
  // Read-only: the user's real login Hand must never be restarted by this test.
  const r = spawnSync('launchctl', ['print', `gui/${process.getuid()}/com.nanocodex.hand`], { encoding: 'utf8' });
  return r.status === 0 ? (r.stdout.match(/^\s*pid = (\d+)/m)?.[1] ?? 'not running') : 'not loaded';
}
// Model a completed CLI selection without restarting the real Hand.
function select(root, key) {
  rmSync(join(root, 'current'));
  symlinkSync(join('versions', key), join(root, 'current'));
  rmSync(join(root, 'pending-update'), { force: true });
}
function keyOf(paths) {
  const h = createHash('sha256');
  for (const p of paths) {
    const bytes = readFileSync(p);
    const size = Buffer.alloc(8);
    size.writeBigUInt64LE(BigInt(bytes.length));
    h.update(size).update(bytes);
  }
  return `local-${h.digest('hex').slice(0, 12)}`;
}
function localKey() {
  const h = createHash('sha256');
  for (const p of [cli, hand]) {
    const bytes = readFileSync(p);
    const size = Buffer.alloc(8);
    size.writeBigUInt64LE(BigInt(bytes.length));
    h.update(size).update(bytes);
  }
  return `local-${h.digest('hex').slice(0, 12)}`;
}
let plist;
let plistBefore;
let linuxOwner;
const livePidBefore = handPid();
trace.push(`live Hand before (read-only launchctl print): ${livePidBefore}`);
try {
  const missingPath = run(runner, ['hand', 'restart', '--executable'], 1);
  assert.match(missingPath.stderr, /value/);
  if (process.platform === 'darwin') {
    const journal = join(store, 'update-transaction.json');
    writeFileSync(journal, 'interrupted development fixture');
    const refused = run(runner, ['hand', 'restart', '--executable', '/nonexistent/development-build'], 1);
    assert.match(refused.stderr, /hand recover/);
    assert.equal(readFileSync(journal, 'utf8'), 'interrupted development fixture');
    rmSync(journal);
  }
  const revision = oneRevision(run(cli, ['--version']).stdout, 'CLI');
  assert.ok(revision, 'CLI must expose full source revision');
  assert.equal(oneRevision(run(hand, ['--version']).stdout, 'Hand'), revision,
    'supply a real CLI + Hand built from the same checkout');
  trace.push(`real candidate pair revision: ${revision}; platform: ${process.platform}; fixture: ${fixture}`);

  if (process.platform === 'darwin') {
    const escape = x => x.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
    plist = join(home, 'Library/LaunchAgents/com.nanocodex.hand.plist');
    mkdirSync(join(home, 'Library/LaunchAgents'), { recursive: true });
    // Not bootstrapped. Native status reads this fixture and, if present, reads
    // the already-existing GUI owner. All test commands defer rather than restart.
    plistBefore = `<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.nanocodex.hand</string><key>ProgramArguments</key><array><string>${escape(hand)}</string><string>hand</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>EnvironmentVariables</key><dict><key>HOME</key><string>${escape(home)}</string><key>NANOCODEX_ACCOUNT_FILE</key><string>${escape(account)}</string></dict></dict></plist>`;
    writeFileSync(plist, plistBefore);
  }

  if (process.platform === 'linux') {
    linuxOwner = JSON.parse(run(runner, ['hand', 'status']).stdout);
    assert.equal(typeof linuxOwner.installed, 'boolean');
    assert.equal(typeof linuxOwner.loaded, 'boolean');
    trace.push(`read-only native Linux owner: ${JSON.stringify(linuxOwner)}`);
  }

  // (b) Real pair installation: the sibling nanocodex-hand is found without
  // --hand-binary; both probes run and their actual bytes are cached.
  update(['--path', cli]);
  const key = localKey();
  const before = active();
  bundleBytes(join(store, 'versions', key), 'update --path CLI (sibling nanocodex-hand)');
  // An explicit --hand-binary elsewhere selects the same pair and key.
  const versionsBeforeExplicit = versions();
  update(['--path', cli, '--hand-binary', explicitHand]);
  assert.deepEqual(versions(), versionsBeforeExplicit, 'explicit --hand-binary must reuse the identical cached pair');
  if (process.platform === 'darwin') {
    assert.equal(pending(), key, 'installed Hand must stage without explicit restart');
    assert.notEqual(before, key, 'CLI must remain on previous version while Hand is deferred');
    update(['--apply']);
    assert.equal(active(), before);
    assert.equal(pending(), key);
    assert.equal(readFileSync(plist, 'utf8'), plistBefore, 'login owner unchanged after updater exits');
    trace.push('observed: native Mac staging + --apply without restart preserve CLI and plist; no persistent-service lifetime acceptance claimed');
  } else {
    if (linuxOwner.installed || linuxOwner.loaded) {
      assert.equal(pending(), key, 'installed Linux owner must defer the pair');
      assert.notEqual(before, key, 'deferred Linux pair must not select the new CLI');
      update(['--apply']);
      assert.equal(active(), before);
      assert.equal(pending(), key);
    } else {
      assert.equal(before, key, 'no-owner Linux pair must activate CLI-only state');
      assert.equal(pending(), null, 'CLI-only activation must clear pending');
    }
    assert.deepEqual(JSON.parse(run(runner, ['hand', 'status']).stdout), linuxOwner,
      'public updater must not mutate the native Linux owner');
    trace.push(`observed Linux disposition: active=${active()}, pending=${pending()}; no root/service mutation requested`);
  }

  // Exercise the public hourly update path offline. Only external downloads
  // are unavailable; real selection, cache, and staging logic still run. The
  // separate CUA refresh may fail harmlessly inside the isolated install root.
  const offline = {
    HTTPS_PROXY: 'http://127.0.0.1:1', https_proxy: 'http://127.0.0.1:1',
    HTTP_PROXY: 'http://127.0.0.1:1', http_proxy: 'http://127.0.0.1:1',
    ALL_PROXY: 'http://127.0.0.1:1', all_proxy: 'http://127.0.0.1:1',
    NO_PROXY: '', no_proxy: '',
  };
  const heldActive = active();
  const heldPending = pending();
  const background = () => run(runner, ['update', '--background'], 0, { env: offline });
  background();
  assert.equal(active(), heldActive, 'hourly update must retain the active selection');
  assert.equal(pending(), heldPending, 'hourly update must not replace the staged local build');
  const heldStatus = update(['--auto', 'status']);
  assert.match(heldStatus.stdout, new RegExp(`explicit selection ${key}`));
  // Model the already completed CLI selection without starting the real Hand.
  // The live service restart/consent journey is deliberately separate.
  rmSync(join(store, 'current'));
  symlinkSync(join('versions', key), join(store, 'current'));
  rmSync(join(store, 'pending-update'), { force: true });
  background();
  assert.equal(active(), key, 'hourly update must retain an active local build');
  assert.equal(pending(), null, 'hourly update must not stage a release over a local build');
  // A later pending selection takes precedence over the active local build.
  // The public status reports this without touching the actual OS scheduler.
  writeFileSync(join(store, 'pending-update'), `${heldActive}\n`);
  if (heldActive !== key) {
    assert.doesNotMatch(update(['--auto', 'status']).stdout, /Background updates paused/);
  }
  rmSync(join(store, 'current'));
  symlinkSync(join('versions', heldActive), join(store, 'current'));
  if (heldPending === null) rmSync(join(store, 'pending-update'), { force: true });
  else writeFileSync(join(store, 'pending-update'), `${heldPending}\n`);
  background();
  trace.push('PASS: public offline background updates preserve staged and active local selections; newer pending selections supersede the hold; no real Hand or scheduler mutation');

  // (c) Hand decoupling. Select the pair as the CLI (modelled: no Hand restart).
  // A CLI-only update (no Hand given or beside --path) keeps that Hand bytes and
  // activates immediately even with an installed owner: nothing is staged and
  // the service is not touched.
  const keepActive = active();
  const keepPending = pending();
  select(store, key);
  const cliKey = keyOf([cliOnly]);
  const cliOnlyRun = update(['--path', cliOnly]);
  assert.match(cliOnlyRun.stderr, /keeping the current Hand/);
  assert.equal(active(), cliKey, 'CLI-only update must activate without a Hand handover');
  assert.equal(pending(), null, 'CLI-only update must not stage a Hand switch');
  bundleBytes(join(store, 'versions', cliKey), 'CLI-only update carries the current Hand');
  if (process.platform === 'darwin') {
    assert.match(cliOnlyRun.stderr, /Hand is unchanged/);
    assert.equal(readFileSync(plist, 'utf8'), plistBefore, 'Hand owner definition unchanged');
  }
  // A full pair whose Hand bytes equal the running Hand also switches only the CLI.
  const samePair = update(['--path', cli]);
  assert.equal(active(), key);
  assert.equal(pending(), null);
  if (process.platform === 'darwin') assert.match(samePair.stderr, /Hand is unchanged/);
  assert.equal(handPid(), livePidBefore, 'live Hand untouched by CLI-only activations');
  trace.push(`PASS (c): CLI-only ${cliKey} and same-Hand pair ${key} activated without staging or service changes; live Hand ${handPid()}`);
  checkAliases(store, revision, 'new updater activation');
  rmSync(join(store, 'current'));
  symlinkSync(join('versions', keepActive), join(store, 'current'));
  if (keepPending !== null) writeFileSync(join(store, 'pending-update'), `${keepPending}\n`);

  if (suppliedOldUpdater) {
    // (a) A previously shipped two-binary updater installs CLI + Hand exactly as
    // it installs a two-asset release. Its bytes are copied read-only.
    const oldStore = join(fixture, 'old-install');
    const oldRunner = join(fixture, 'old-runner', 'nanocodex');
    mkdirSync(oldStore, { recursive: true });
    mkdirSync(dirname(oldRunner), { recursive: true });
    copyFileSync(suppliedOldUpdater, oldRunner);
    chmodSync(oldRunner, 0o755);
    writeFileSync(join(oldStore, 'automatic-updates-disabled'), '');
    const oldEnv = { env: { NANOCODEX_DIR: oldStore } };
    trace.push(`old updater: ${suppliedOldUpdater} sha256 ${digest(readFileSync(oldRunner))}\n${run(oldRunner, ['--version']).stdout}`);
    run(oldRunner, ['update', '--path', cli, '--hand-binary', hand], 0, oldEnv);
    const oldKey = localKey();
    bundleBytes(join(oldStore, 'versions', oldKey), 'old updater two-name install');
    select(oldStore, oldKey);
    for (const alias of ['nanocodex', 'nanocodex2']) {
      assert.equal(oneRevision(run(join(oldStore, 'bin', alias), ['--version'], 0, oldEnv).stdout,
        `old-updater bin/${alias}`), revision);
    }
    trace.push(`PASS (a) old-updater layout: bin/nanocodex -> ${readlinkSync(join(oldStore, 'bin', 'nanocodex'))}, bin/nanocodex2 -> ${readlinkSync(join(oldStore, 'bin', 'nanocodex2'))} both run revision ${revision}`);
    // The new CLI takes over the same cached pair: the Hand is unchanged, so
    // it activates directly and publishes the unified entrypoints.
    const takeover = run(runner, ['update', '--path', join(oldStore, 'versions', oldKey, 'nanocodex'),
      '--hand-binary', join(oldStore, 'versions', oldKey, 'nanocodex2')], 0, oldEnv);
    assert.equal(basename(readlinkSync(join(oldStore, 'current'))), oldKey);
    if (process.platform === 'darwin') assert.match(takeover.stderr, /Hand is unchanged/);
    checkAliases(oldStore, revision, 'old updater install taken over by the new CLI');
  } else {
    trace.push('SKIPPED (a): no --old-updater supplied');
  }

  // Executable rejection fixture, not a fake updater/Hand service. Failure of
  // --version is exercised through the real public local-pair probe.
  const failing = join(fixture, 'pair', 'failing-probe');
  writeFileSync(failing, '#!/bin/sh\nexit 19\n', { mode: 0o755 });
  const snapshot = versions();
  const oldActive = active();
  const oldPending = pending();
  const bad = update(['--path', cli, '--hand-binary', failing], 1);
  assert.match(bad.stderr, /version probe failed/);
  assert.deepEqual(versions(), snapshot, 'failed pair must not install a candidate version');
  assert.equal(active(), oldActive);
  assert.equal(pending(), oldPending);
  assert.match(update(['--auto', 'status']).stdout, new RegExp(`explicit selection ${key}`), 'failed install must preserve the prior hold');
  const mismatched = join(fixture, 'pair', 'mismatched-probe');
  const different = revision === '0'.repeat(40) ? '1'.repeat(40) : '0'.repeat(40);
  writeFileSync(mismatched, `#!/bin/sh\nprintf 'nanocodex2 Version: fixture\\nCommit SHA: ${different}\\n'\n`, { mode: 0o755 });
  const mismatch = update(['--path', cli, '--hand-binary', mismatched], 1);
  assert.match(mismatch.stderr, /source revision .* differs from Hand/);
  assert.deepEqual(versions(), snapshot);
  assert.equal(active(), oldActive);
  assert.equal(pending(), oldPending);
  update(['--path', cli, '--branch', 'topic'], 1);
  update(['--pr', '0'], 1);
  update(['--path', cli, '--hand-binary', join(fixture, 'missing')], 1);

  // Inject an interrupted staged bundle via on-disk public installation state,
  // then verify the real --apply boundary fails closed. This does NOT claim a
  // background release download or actual service rollback was exercised.
  writeFileSync(join(store, 'pending-update'), `${key}\n`);
  const cachedHand = join(store, 'versions', key, 'nanocodex2');
  const original = readFileSync(cachedHand);
  writeFileSync(cachedHand, 'corrupt companion');
  const corrupt = update(['--apply'], 1);
  assert.match(corrupt.stderr, /checksum|incomplete|corrupt/i);
  assert.equal(active(), oldActive);
  assert.equal(pending(), key);
  writeFileSync(cachedHand, original);
  chmodSync(cachedHand, 0o755);

  if (process.platform === 'darwin') {
    // Model a crash after CLI activation but before a coordinator commit. No
    // Hand backup exists, so native recovery does not stop/start a service.
    const journal = join(store, 'update-transaction.json');
    rmSync(join(store, 'current'));
    symlinkSync(join('versions', key), join(store, 'current'));
    writeFileSync(journal, JSON.stringify({ previous: oldActive, candidate: key }));
    run(runner, ['hand', 'recover']);
    assert.equal(active(), oldActive);
    assert.equal(existsSync(journal), false);
    assert.equal(readFileSync(plist, 'utf8'), plistBefore);

    // Recovery's committed CLI-only branch is safe in the real user's GUI
    // domain: service:false never ensures/starts an OS publisher. Both records
    // below model crashes; they are not real process-kill fault injections.
    writeFileSync(journal, JSON.stringify({ previous: oldActive, candidate: oldActive, phase: 'committed', service: false }));
    run(runner, ['hand', 'recover']);
    assert.equal(existsSync(journal), false);
    assert.equal(active(), oldActive);
    writeFileSync(journal, JSON.stringify({ previous: oldActive, candidate: key, phase: 'committed', service: false }));
    const refused = run(runner, ['hand', 'recover'], 1);
    assert.match(refused.stderr, /no longer matches the active CLI/);
    assert.equal(existsSync(journal), true, 'ambiguous recovery evidence retained');
    rmSync(journal);
    assert.equal(readFileSync(plist, 'utf8'), plistBefore);
  }
  if (process.platform === 'linux') {
    // Real Git fetch into the real updater. This minimal historical source lacks
    // the exact-source packaging contract and MUST fail before Cargo/activation.
    // It is not a substitute for building the production Linux helper payload.
    const source = join(fixture, 'historical-source');
    const remote = join(fixture, 'historical-source.git');
    mkdirSync(source);
    run('git', ['init', '--bare', remote]);
    run('git', ['-C', source, 'init', '-b', 'historical']);
    writeFileSync(join(source, 'Cargo.toml'), '[workspace]\nresolver = "2"\nmembers = []\n');
    run('git', ['-C', source, 'add', '.']);
    run('git', ['-C', source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'historical packaging fixture']);
    run('git', ['-C', source, 'push', remote, 'HEAD:refs/heads/historical']);
    env.GIT_CONFIG_COUNT = '1';
    env.GIT_CONFIG_KEY_0 = `url.${remote}.insteadOf`;
    env.GIT_CONFIG_VALUE_0 = 'https://github.com/gakonst/nanocodex.git';
    try {
      const historical = update(['--branch', 'historical'], 1);
      assert.match(historical.stderr, process.arch === 'x64'
        ? /predates the self-contained Linux screen-helper packaging contract/
        : /self-contained Linux source updates require x86_64/);
      assert.doesNotMatch(historical.stderr, /compiling nanocodex/);
      assert.deepEqual(versions(), snapshot);
      assert.equal(active(), oldActive);
      assert.equal(pending(), key);
    } finally {
      delete env.GIT_CONFIG_COUNT;
      delete env.GIT_CONFIG_KEY_0;
      delete env.GIT_CONFIG_VALUE_0;
    }
    assert.deepEqual(JSON.parse(run(runner, ['hand', 'status']).stdout), linuxOwner);
    trace.push(`Linux source preflight (${process.arch}) rejected before Cargo; no production source/helper packaging acceptance claimed`);
  }
  if (withSource && process.platform === 'darwin') {
    // The existing source fixture inherits only our already-clean environment.
    // The shipped identity helper uses cached public Cargo dependencies. Share
    // only that registry cache, keeping Cargo configuration and credentials out
    // of the fixture, and reuse the installed non-secret rustup toolchain.
    const sourceOutput = join(output, 'mac-source');
    const cargoHome = join(home, 'source-cargo');
    mkdirSync(sourceOutput, { recursive: true });
    mkdirSync(cargoHome);
    symlinkSync(join(process.env.CARGO_HOME ?? join(process.env.HOME, '.cargo'), 'registry'),
      join(cargoHome, 'registry'), 'dir');
    const sourceRunner = join(dirname(fileURLToPath(import.meta.url)), 'update_source_e2e.mjs');
    run(process.execPath, [sourceRunner, runner], 0, {
      cwd: sourceOutput, timeout: 900_000,
      env: {
        CARGO_HOME: cargoHome,
        CARGO_NET_OFFLINE: 'true',
        RUSTUP_HOME: process.env.RUSTUP_HOME ?? join(process.env.HOME, '.rustup'),
      },
    });
    assert.equal(readFileSync(plist, 'utf8'), plistBefore);
    trace.push(`macOS source-selector journeys used real Git/Cargo, minimal source inputs and gh metadata fixture; isolated account/Cargo; transcript: ${join(sourceOutput, 'output/update-source-e2e/transcript.log')}`);
  } else if (withSource) {
    trace.push('Linux source fixture intentionally exercises preflight rejection only; the macOS minimal fixture is not Linux packaging success acceptance');
  }
  assert.deepEqual(readFileSync(account), accountBefore, 'synthetic account must not be rewritten');
  const livePidAfter = handPid();
  trace.push(`live Hand after (read-only launchctl print): ${livePidAfter}`);
  assert.equal(livePidAfter, livePidBefore, 'the real Hand must not be restarted');
  verdict = 'PASSED';
  trace.push('scope: real updater, real candidate probes/bundle bytes, staging, rejection and corruption; synthetic recovery records. NOT coverage: release HTTP downloads, running Hand handover/rollback, Windows execution, post-logout/reboot lifetime.');
  process.stdout.write(`local updater journeys passed; transcript: ${join(output, 'transcript.log')}\n`);
} finally {
  writeFileSync(join(output, 'transcript.log'), `${trace.join('\n\n')}\n\nverdict: ${verdict}\n`);
  rmSync(fixture, { recursive: true, force: true });
}
