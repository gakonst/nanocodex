// Opt-in native macOS journey against an already connected launchd owner.
// Never installs a fixture service or requests a restart. No account API calls
// or credentials are needed: the real owner's published catalog is the input.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

assert.equal(process.platform, 'darwin', 'This journey requires native macOS launchd.');
assert.ok(process.argv[2], 'usage: node bin/nanocodex/tests/hand_install_e2e.mjs CLI [OUTPUT_DIR]');
const output = resolve(process.argv[3] ?? 'output/hand-install-e2e');
mkdirSync(output, { recursive: true });
const fixture = mkdtempSync(join(tmpdir(), 'nanocodex hand install '));
const cli = join(fixture, 'nanocodex');
copyFileSync(resolve(process.argv[2]), cli);
const store = join(fixture, 'install');
mkdirSync(store);
writeFileSync(join(store, 'automatic-updates-disabled'), '');
const account = join(fixture, 'synthetic-account.json');
writeFileSync(account, JSON.stringify({ fixture: true }), { mode: 0o600 });
const env = {
  PATH: process.env.PATH, HOME: homedir(), NANOCODEX_DIR: store,
  NANOCODEX_ACCOUNT_FILE: account, NO_COLOR: '1',
  TMPDIR: process.env.TMPDIR ?? tmpdir(),
};
const transcript = [];
let verdict = 'FAILED';
function run(args, expected = 0) {
  const start = performance.now();
  const result = spawnSync(cli, args, {
    cwd: fixture, env, encoding: 'utf8', timeout: 10_000,
  });
  transcript.push(`$ nanocodex ${args.join(' ')}\nexpected exit: ${expected}\nobserved exit: ${result.status}; elapsed: ${Math.round(performance.now() - start)} ms; error: ${result.error?.message ?? 'none'}\nstdout:\n${result.stdout ?? ''}\nstderr:\n${result.stderr ?? ''}`);
  writeFileSync(join(output, 'transcript.log'), transcript.join('\n\n'));
  assert.equal(result.error, undefined, 'CLI must finish within 10 seconds');
  assert.equal(result.status, expected, result.stderr);
  return result;
}
const status = () => JSON.parse(run(['hand', 'status']).stdout);
try {
  const before = status();
  assert.ok(before.installed && before.loaded && before.pid, 'Start with an installed, running native Hand.');
  const executable = realpathSync(before.executable);
  symlinkSync(executable, join(fixture, 'nanocodex2'));
  const hands = join(homedir(), '.nanocodex/hands');
  const catalog = readdirSync(hands).flatMap(name => {
    try { return [JSON.parse(readFileSync(join(hands, name, 'status.json'), 'utf8'))]; }
    catch { return []; }
  }).find(value => value.status === 'connected'
    && value.daemon?.pid === before.pid && value.daemon?.executable === executable);
  assert.ok(catalog, 'The exact launchd owner must already be connected; this journey must not request recovery.');
  const plist = join(homedir(), 'Library/LaunchAgents/com.nanocodex.hand.plist');
  const definition = readFileSync(plist);
  transcript.push(`native input: connected owner pid=${before.pid}; screen=${catalog.screen?.status ?? 'absent'}; no service-manager fixtures; synthetic invalid account override`);

  for (const options of [[], ['--executable', executable]]) {
    const installed = run(['hand', 'install', ...options]);
    assert.match(installed.stderr, /installed and connected/);
    if (catalog.screen?.status !== 'ready' || catalog.screen?.transport !== 'webrtc') {
      assert.match(installed.stderr, /screen sharing is unavailable or still starting/);
    }
    assert.deepEqual(status(), before, 'Installer exit must preserve the exact persistent owner.');
    assert.deepEqual(readFileSync(plist), definition, 'Connected service configuration must remain unchanged.');
  }
  // Candidate validation errors must also finish promptly and preserve the owner.
  const missing = run(['hand', 'install', '--executable', join(fixture, 'missing')], 1);
  assert.match(missing.stderr, /executable is missing/);
  const directory = run(['hand', 'install', '--executable', fixture], 1);
  assert.match(directory.stderr, /Expected a regular file/);
  assert.deepEqual(status(), before);
  assert.deepEqual(readFileSync(plist), definition);
  assert.deepEqual(JSON.parse(readFileSync(account, 'utf8')), { fixture: true });
  transcript.push('Observed: repeated installs complete without restarting the real connected owner, unavailable screen is warned about, candidate errors preserve the owner and configuration. Fresh install, account rejection and version-handover acceptance require a disposable desktop user.');
  verdict = 'PASS';
} finally {
  transcript.push(verdict);
  writeFileSync(join(output, 'transcript.log'), `${transcript.join('\n\n')}\n`);
  rmSync(fixture, { recursive: true, force: true });
}
console.log(`${verdict}: ${join(output, 'transcript.log')}`);
