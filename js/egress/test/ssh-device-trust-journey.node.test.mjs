import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { createServer } from 'node:net';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir, userInfo } from 'node:os';
import { join } from 'node:path';
import { build } from 'esbuild';
const require = createRequire(import.meta.url);
const { Miniflare, convertV4MiniflareOptions, Log, LogLevel } = createRequire(require.resolve('wrangler/package.json'))('miniflare');
const root = new URL('../', import.meta.url).pathname;
const SSHD = process.env.NANOCODEX_TEST_SSHD ?? '/usr/sbin/sshd';
const TARGET = 'sshd.example.com';
// A presented host key outside the accepted set aborts the SSH handshake before
// user authentication; the command reports it as an SSH failure (exit 255).
const REJECTED = { status: 200, exit_code: 255 };

// The production egress Worker, its encrypted credential broker DO, the real
// managed HandDeviceSshHostKeys entrypoint and a real OpenSSH sshd run here.
// Fixtures: TARGET's DNS answer (the TCP shim connects it to loopback), and the
// account DO's deviceSshHostKeys contract (its revoke/rotate semantics are
// covered by the managed hand-device journey).
test('SSH recovery trusts device-attested host keys only for the bound active device', { timeout: 180_000, skip: !existsSync(SSHD) && !process.env.CI && 'OpenSSH sshd is unavailable (required in CI)' }, async () => {
  const work = await mkdtemp(join(tmpdir(), 'ncx-ssh-device-trust-'));
  const evidence = process.env.NANOCODEX_EVIDENCE_DIR;
  const trace = [];
  const logs = [];
  let sshd, mf;
  try {
    const port = await freePort();
    for (const name of ['host_a', 'host_b', 'host_other']) execFileSync('ssh-keygen', ['-q', '-t', 'ecdsa', '-b', '256', '-N', '', '-C', name, '-f', join(work, name)]);
    const fingerprint = name => execFileSync('ssh-keygen', ['-l', '-E', 'sha256', '-f', join(work, name + '.pub')]).toString().split(' ')[1];
    const fpA = fingerprint('host_a'), fpB = fingerprint('host_b'), fpOther = fingerprint('host_other');
    await writeFile(join(work, 'authorized_keys'), '', { mode: 0o600 });
    const startSshd = async hostKey => {
      await writeFile(join(work, 'sshd_config'), [
        'Port ' + port, 'ListenAddress 127.0.0.1', 'HostKey ' + join(work, hostKey), 'PidFile ' + join(work, 'sshd.pid'),
        'AuthorizedKeysFile ' + join(work, 'authorized_keys'), 'StrictModes no', 'UsePAM no', 'PasswordAuthentication no',
        'KbdInteractiveAuthentication no', 'PubkeyAuthentication yes', 'AllowUsers ' + userInfo().username, '',
      ].join('\n'));
      const child = spawn(SSHD, ['-D', '-e', '-f', join(work, 'sshd_config')], { stdio: ['ignore', 'ignore', 'pipe'] });
      child.stderr.on('data', data => logs.push('sshd: ' + String(data).trim()));
      await waitForPort(port);
      return child;
    };
    const stopSshd = async () => { if (!sshd) return; const done = new Promise(resolve => sshd.once('exit', resolve)); sshd.kill('SIGTERM'); await done; sshd = undefined; };

    const egress = await bundle(root + 'src/egress.ts', [{ name: 'tcp-dns-fixture', setup(b) {
      b.onResolve({ filter: /^cloudflare:sockets$/ }, args => args.importer.endsWith('tcp-dns-fixture.mjs')
        ? { path: 'cloudflare:sockets', external: true } : { path: root + 'test/fixtures/tcp-dns-fixture.mjs' });
      b.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: './nanocodex.wasm', external: true }));
      b.onResolve({ filter: /^\.\/whatsapp-runtime$/ }, () => ({ path: root + 'test/whatsapp/runtime.fixture.ts' }));
      // Model-provider WASM glue is unused by SSH; an empty module stands in when it is not built.
      if (!existsSync(root + '../nanocodex/pkg-web/nanocodex.js')) {
        b.onResolve({ filter: /\/pkg-web\/nanocodex\.js$/ }, () => ({ path: 'pkg-web-absent', namespace: 'unbuilt' }));
        b.onLoad({ filter: /.*/, namespace: 'unbuilt' }, () => ({ contents: 'module.exports = {};', loader: 'js' }));
      }
    } }]);
    const managed = await bundle(root + 'test/fixtures/hand-device-ssh-host-keys.worker.mjs', []);
    const wasm = process.env.NANOCODEX_TEST_WASM ?? root + '../nanocodex/pkg-web/nanocodex_bg.wasm';
    class CapturedLog extends Log { logWithLevel(_level, message) { logs.push(String(message)); } }
    mf = new Miniflare(convertV4MiniflareOptions({
      log: new CapturedLog(LogLevel.DEBUG),
      handleStructuredLogs: log => { logs.push(JSON.stringify(log)); },
      workers: [{
        name: 'egress',
        modules: [
          { type: 'ESModule', path: root + 'output/ssh-device-trust-egress.js', contents: egress },
          // The Claude WASM module is unused by SSH; a minimal module stands in when it is not built.
          { type: 'CompiledWasm', path: root + 'output/nanocodex.wasm', contents: existsSync(wasm) ? await readFile(wasm) : Uint8Array.from([0, 97, 115, 109, 1, 0, 0, 0]) },
        ],
        compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'],
        bindings: { ENVIRONMENT: 'test', CREDENTIAL_ENCRYPTION_KEY: Buffer.from('0123456789abcdef0123456789abcdef').toString('base64url') },
        serviceBindings: { HAND_DEVICE_SSH_HOST_KEYS: { name: 'managed', entrypoint: 'HandDeviceSshHostKeys' } },
        durableObjects: Object.fromEntries(Object.entries({ USER_CREDENTIALS: 'UserCredentialBroker', AGENT_SUBJECTS: 'AgentSubjectDirectory', USER_CONNECTORS: 'UserConnectorBroker', MCP_CONNECTIONS: 'McpConnectionDirectory', WHATSAPP_ACCOUNTS: 'WhatsAppAccount', SPOTIFY_RATE_LIMITS: 'SpotifyRateLimit', GMAIL_PUSH_MAILBOXES: 'GmailPushMailbox' }).map(([key, className]) => [key, { className, useSQLite: true }])),
      }, {
        name: 'managed',
        modules: [{ type: 'ESModule', path: root + 'output/ssh-device-trust-managed.js', contents: managed }],
        compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'],
        durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: 'AccountHostedTools', useSQLite: true } },
      }],
    }));
    const owner = '11111111-1111-4111-8111-111111111111', subject = 'S'.repeat(43);
    const publicKeys = new Map();
    const user = userInfo().username;
    const call = async (url, method, body, headers = {}) => {
      const response = await mf.dispatchFetch(url, { method, headers: { 'content-type': 'application/json', ...headers }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const text = await response.text();
      return { status: response.status, body: text ? JSON.parse(text) : null };
    };
    const managedWorker = await mf.getWorker('managed');
    const devices = async state => {
      const response = await managedWorker.fetch('https://fixture.internal/devices?owner=' + owner, { method: 'PUT', body: JSON.stringify(state) });
      assert.equal(response.status, 204);
    };
    const saveTarget = async (reference, target) => {
      await call('https://broker.internal/users/' + owner + '/credentials/ssh/' + reference, 'DELETE');
      // The broker stamps saved_at while handling the PUT, strictly after this.
      const before = Date.now() - 1;
      // The Vault generates the client key; only its public half is installed on sshd.
      const saved = await call('https://broker.internal/users/' + owner + '/credentials/ssh/' + reference, 'PUT', { generate: true, hostname: TARGET, port, username: user, ...target });
      assert.equal(saved.status, 204, JSON.stringify(saved.body));
      const listed = await call('https://broker.internal/users/' + owner + '/credentials', 'GET');
      const entry = listed.body.ssh.find(identity => identity.reference === reference);
      assert.deepEqual(Object.keys(entry).sort(), ['hostname', 'port', 'public_key', 'reference', 'username', ...Object.keys(target)].sort());
      publicKeys.set(reference, entry.public_key);
      await writeFile(join(work, 'authorized_keys'), [...publicKeys.values()].join('\n') + '\n', { mode: 0o600 });
      return before;
    };
    const ssh = async (scenario, body, expected) => {
      const before = logs.length;
      const result = await call('https://ssh.internal/v1/execute', 'POST', { hostname: TARGET, port, username: user, command: ['printf', 'reached-%s', 'sshd'], ...body }, { 'x-nanocodex-subject': subject });
      // workerd prints the structured audit record in util.inspect form, and
      // delivers it to the log handler shortly after the response.
      const auditLine = () => logs.slice(before).some(line => line.includes("rule: 'ssh'"));
      for (let attempt = 0; attempt < 40 && !auditLine(); attempt++) await new Promise(resolve => setTimeout(resolve, 25));
      const audit = logs.slice(before).map(line => { try { return JSON.parse(line).message; } catch { return undefined; } })
        .filter(message => typeof message === 'string' && message.includes("type: 'egress.request'") && message.includes("rule: 'ssh'"))
        .map(message => Object.fromEntries([...message.matchAll(/^\s+(\w+): (?:'([^']*)'|(\d+))/gm)].map(([, key, text, number]) => [key, text ?? Number(number)])))[0];
      const auditRecord = audit && Object.fromEntries(['action', 'code', 'status', 'host_key_trust_mode', 'attested_host_keys', 'host_key_source', 'host_key_sha256'].filter(key => key in audit).map(key => [key, audit[key]]));
      trace.push({ scenario, request: body, expected, observed: { status: result.status, body: result.body }, audit: auditRecord });
      assert.equal(result.status, expected.status, scenario + ': ' + JSON.stringify(result.body));
      if (expected.exit_code === 255) {
        assert.equal(result.body.exit_code, 255, scenario);
        assert.equal(result.body.stdout, '', scenario);
        assert.match(result.body.stderr, /server host-key authentication failed/, scenario);
      } else if (expected.status === 200) assert.deepEqual([result.body.exit_code, result.body.stdout], [0, 'reached-sshd'], scenario);
      if (expected.error) assert.equal(result.body.error, expected.error, scenario);
      return { result, audit: auditRecord };
    };
    const serverMachine = reference => 'server:' + serverHandID(owner, reference);
    const device = (id, createdAt, keys) => ({ device_id: id, created_at: createdAt, key_version: 1, host_keys: keys.map(([fingerprint, attestedAt]) => ({ fingerprint, attested_at: attestedAt })) });
    const D1 = '22222222-2222-4222-8222-222222222222', D2 = '33333333-3333-4333-8333-333333333333', D3 = '44444444-4444-4444-8444-444444444444';

    assert.equal((await call('https://broker.internal/subjects/' + subject, 'PUT', { user_id: owner })).status, 200);
    sshd = await startSshd('host_a');
    const savedSrv = await saveTarget('srv', { host_key_sha256: fpA });
    await devices({});
    // Initial bootstrap: the user-authorized Vault pin is the host authority.
    assert.equal((await ssh('pinned target, no opt-in, sshd key A', { identity_ref: 'srv' }, { status: 200 })).audit.host_key_source, 'vault_pin');
    // The body can only opt in; a caller-supplied host key is rejected outright.
    await ssh('client-injected fingerprint', { identity_ref: 'srv', host_key_trust: 'device', device_host_keys: [{ fingerprint: fpB, attested_at: Date.now() }] }, { status: 400, error: 'invalid_ssh_request' });
    await ssh('client-injected pin', { identity_ref: 'srv', host_key_sha256: fpB }, { status: 400, error: 'invalid_ssh_request' });
    await ssh('unknown trust mode', { identity_ref: 'srv', host_key_trust: 'hand:attacker' }, { status: 400, error: 'invalid_ssh_request' });

    // sshd host key rotates to B. Without a device attestation recovery fails closed.
    await stopSshd(); sshd = await startSshd('host_b');
    await ssh('rotated key B, no opt-in', { identity_ref: 'srv' }, REJECTED);
    await ssh('rotated key B, opt-in, no device', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);
    // A different machine's active device attesting B never vouches for srv.
    await devices({ [serverMachine('other')]: device(D2, Date.now() + 1, [[fpB, Date.now()]]), 'laptop-1': device(D3, Date.now() + 1, [[fpB, Date.now()]]) });
    await ssh('rotated key B, attested only by other machines', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);
    // A device bootstrapped through an earlier snapshot of this target does not count.
    await devices({ [serverMachine('srv')]: device(D1, savedSrv - 60_000, [[fpB, Date.now()]]) });
    await ssh('rotated key B, device enrolled before the Vault target snapshot', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);
    // An attestation made before the target was saved does not count.
    await devices({ [serverMachine('srv')]: device(D1, Date.now() + 1, [[fpB, savedSrv - 1]]) });
    await ssh('rotated key B, attestation older than the Vault target', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);
    // The bound active device re-attests B: recovery succeeds without editing the Vault.
    await devices({ [serverMachine('srv')]: device(D1, Date.now() + 1, [[fpOther, Date.now()], [fpB, Date.now()]]) });
    const recovered = await ssh('rotated key B, attested by the bound active device', { identity_ref: 'srv', host_key_trust: 'device' }, { status: 200 });
    assert.equal(recovered.audit.host_key_source, 'device:' + D1);
    assert.equal(recovered.audit.host_key_sha256, fpB);
    await ssh('rotated key B, attested, but no opt-in', { identity_ref: 'srv' }, REJECTED);
    // Revoked or forgotten device: the account DO returns no active device.
    await devices({});
    await ssh('rotated key B, device revoked', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);
    // Mutating the Vault target (delete + re-add) invalidates prior attestations.
    await devices({ [serverMachine('srv')]: device(D1, Date.now() + 1, [[fpB, Date.now()]]) });
    await saveTarget('srv', { host_key_sha256: fpA });
    await ssh('rotated key B, Vault target re-created after the attestation', { identity_ref: 'srv', host_key_trust: 'device' }, REJECTED);

    // A target bound to an exact enrolled Hand needs no manual fingerprint.
    const naked = await call('https://broker.internal/users/' + owner + '/credentials/ssh/naked', 'PUT', { generate: true, hostname: TARGET, port, username: user });
    trace.push({ scenario: 'Vault target with neither pin nor device binding', expected: { status: 400 }, observed: naked });
    assert.equal(naked.status, 400);
    const savedLaptop = await saveTarget('laptop', { host_key_trust: 'hand:laptop-1' });
    await devices({});
    await ssh('unpinned hand-bound target, no attestation', { identity_ref: 'laptop' }, { status: 403, error: 'ssh_host_key_unattested' });
    await devices({ 'laptop-2': device(D2, Date.now() + 1, [[fpB, Date.now()]]), [serverMachine('laptop')]: device(D3, Date.now() + 1, [[fpB, Date.now()]]) });
    await ssh('unpinned hand-bound target, other devices attest', { identity_ref: 'laptop' }, { status: 403, error: 'ssh_host_key_unattested' });
    await devices({ 'laptop-1': device(D2, savedLaptop - 86_400_000, [[fpA, Date.now()]]) });
    await ssh('unpinned hand-bound target, bound device attests a different key', { identity_ref: 'laptop' }, REJECTED);
    await devices({ 'laptop-1': device(D2, savedLaptop - 86_400_000, [[fpB, Date.now()]]) });
    const bound = await ssh('unpinned hand-bound target, bound device attests B', { identity_ref: 'laptop' }, { status: 200 });
    assert.equal(bound.audit.host_key_source, 'device:' + D2);

    const joined = logs.join('\n');
    assert.equal(joined.includes('PRIVATE KEY'), false, 'logs must not contain key material');
    if (evidence) {
      await mkdir(evidence, { recursive: true });
      await writeFile(join(evidence, 'ssh-device-trust-trace.json'), JSON.stringify({ sshd: String(spawnSync(SSHD, ['-V']).stderr).split('\n').find(line => line.startsWith('OpenSSH')), port, fingerprints: { A: fpA, B: fpB, other: fpOther }, owner, machines: { srv: serverMachine('srv'), laptop: 'laptop-1' }, trace }, null, 2));
      await writeFile(join(evidence, 'ssh-device-trust-logs.txt'), logs.filter(line => /"rule":"ssh"|sshd:/.test(line)).join('\n'));
    }
  } finally {
    if (evidence) {
      await mkdir(evidence, { recursive: true });
      await writeFile(join(evidence, 'ssh-device-trust-all-logs.txt'), logs.join('\n'));
    }
    await mf?.dispose();
    if (sshd) sshd.kill('SIGTERM');
    await rm(work, { recursive: true, force: true });
  }
});

async function bundle(entry, plugins) {
  // The SSH key libraries are CommonJS and require Node built-ins, which
  // workerd provides under nodejs_compat; give the ESM bundle a real require.
  const result = await build({ entryPoints: [entry], bundle: true, write: false, format: 'esm', platform: 'node',
    banner: { js: "import { createRequire as __ncxCreateRequire } from 'node:module'; const require = __ncxCreateRequire('/');" },
    external: ['cloudflare:*', 'node:*'], alias: { 'node-rsa': root + '../nanocodex/tools/browser/unsupportedNodeRsa.mjs' }, plugins });
  return result.outputFiles[0].text;
}

function serverHandID(owner, reference) {
  const bytes = createHash('sha256').update('server-hand:v1\0' + owner + '\0' + reference).digest().subarray(0, 16);
  bytes[6] = (bytes[6] & 15) | 64; bytes[8] = (bytes[8] & 63) | 128;
  const hex = bytes.toString('hex');
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-');
}

function freePort() {
  return new Promise((resolve, reject) => { const server = createServer(); server.listen(0, '127.0.0.1', () => { const { port } = server.address(); server.close(() => resolve(port)); }); server.on('error', reject); });
}

async function waitForPort(port) {
  const { connect } = await import('node:net');
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await new Promise(resolve => { const socket = connect(port, '127.0.0.1', () => { socket.destroy(); resolve(true); }); socket.on('error', () => resolve(false)); })) return;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error('sshd did not listen');
}
