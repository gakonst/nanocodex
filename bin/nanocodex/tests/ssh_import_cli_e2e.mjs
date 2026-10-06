// Actual legacy executable -> HTTP device approval -> SSH credential import.
// cargo build -p nanocodex-bin --bin nanocodex
// node bin/nanocodex/tests/ssh_import_cli_e2e.mjs /absolute/path/to/nanocodex
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, generateKeyPairSync } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, readFile, writeFile, mkdir, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const binary = resolve(process.argv[2] || 'target/debug/nanocodex');
const directory = await mkdtemp(join(tmpdir(), 'nanocodex-ssh-import-'));
// Generated for this isolated fixture; no real keys/accounts are read.
const { privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
const pem = privateKey.export({ format: 'pem', type: 'sec1' }).toString();
const keyFile = join(directory, 'synthetic.pem');
await writeFile(keyFile, pem, { mode: 0o600 });
const pin = `SHA256:${Buffer.alloc(32, 1).toString('base64').replace(/=+$/, '')}`;
const imported = { reference: 'synthetic-server', hostname: 'host.example', port: 22, username: 'deploy', host_key_sha256: pin, private_key: pem };
function commitment(value) {
  const hash = createHash('sha256').update('nanocodex/ssh-credential-import/v1\0');
  for (const field of ['reference', 'hostname', 'username', 'host_key_sha256', 'private_key']) {
    const bytes = Buffer.from(value[field]);
    const length = Buffer.alloc(4); length.writeUInt32BE(bytes.length);
    hash.update(length).update(bytes);
  }
  const port = Buffer.alloc(4); port.writeUInt32BE(value.port); hash.update(port);
  return `urn:nanocodex:credential-import:ssh:pem-v1:sha256:${hash.digest('base64url')}`;
}
const target = `urn:nanocodex:ssh-target:${encodeURIComponent(imported.reference)}:${encodeURIComponent(imported.hostname)}:22:${encodeURIComponent(imported.username)}:${encodeURIComponent(pin)}`;
const capabilities = ['nanocodex.agent', 'agent.output.final', 'agent.output.actions', 'agent.history.read', 'history:read', 'memory:read', 'memory:write', 'github'];
const account = `0x${'11'.repeat(20)}`;
let mode = 'success', requests = [], approved = false, sequence = 0, priorConnection;
const grants = new Map();
let serverFailure;
const server = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const bytes = Buffer.concat(chunks);
    const body = bytes.length ? JSON.parse(bytes) : null;
    requests.push({ method: request.method, path: request.url, body });
    const send = (status, body) => response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    if (request.method === 'GET' && request.url.startsWith('/v1/grants/')) return send(200, grants.get(request.url.split('/')[3]));
    if (request.url.endsWith('/revoke')) return send(200, { ...grants.get(request.url.split('/')[3]).grant, status: 'revoked' });
    if (request.url === '/v1/device/register') {
      approved = false;
      const resources = body.message.payload[0].params[0].capabilities.auth.resources;
      assert.ok(!JSON.stringify(body).includes('PRIVATE KEY'));
      if (mode !== 'setup') {
        assert.equal(resources.filter(r => r.startsWith('urn:nanocodex:credential-import:ssh:')).length, 1);
        assert.ok(resources.includes(commitment(imported)));
        assert.ok(resources.includes(target));
      }
      return send(200, { device_code: 'device_code_123456789', user_code: 'ABCD-EFGH', verification_uri: `${origin}/v1/device/verify`, verification_uri_complete: `${origin}/v1/device/verify?user_code=ABCDEFGH`, expires_in: 60, interval: 1 });
    }
    if (request.url === '/v1/device/token') {
      assert.ok(!JSON.stringify(body).includes('PRIVATE KEY'));
      approved = true;
      return send(200, { type: 'rpc-responses', payload: [{ jsonrpc: '2.0', id: 'nanocodex-cli-login', result: { accounts: [{ address: account, capabilities: { auth: { approval_id: 'a'.repeat(43), mode: 'hosted' } } }] } }] });
    }
    assert.equal(request.url, '/v1/connections');
    assert.equal(approved, true, 'private key sent before approval');
    assert.equal(request.headers.origin, 'https://cli.nanocodex.xyz');
    assert.equal(request.headers['x-nanocodex-app-id'], 'nanocodex-cli');
    if (mode !== 'setup') {
      assert.deepEqual(body.ssh_credential_import, imported);
      assert.ok(!body.requested_connectors.includes('ssh'));
    }
    if (mode === 'ambiguous') { request.socket.destroy(); return; }
    if (mode === 'remote-error') return send(403, { error: { code: 'connector_not_connected', message: pem } });
    sequence++;
    priorConnection = { authorization_mode: 'hosted', grant_token: String(sequence).repeat(43), account_id: '123e4567-e89b-42d3-a456-426614174000', account_address: account, agent_id: 'agent_hosted', grant: { id: `0x${String(sequence).repeat(64)}`, permission: 'agent.run', status: 'active', expires_at: Math.floor(Date.now()/1000)+86400, capabilities }, mpp: {} };
    grants.set(priorConnection.grant.id, priorConnection);
    send(201, priorConnection);
  } catch (error) { serverFailure = error; response.writeHead(500).end('{}'); }
});
await new Promise(resolve => server.listen(0, '::1', resolve));
const origin = `http://nanocodex.localhost:${server.address().port}`;
const baseArgs = ['--device-base-url', `${origin}/v1/device`, '--no-open'];
const sshArgs = ['connect', 'ssh', '--key-file', keyFile, '--reference', imported.reference, '--hostname', imported.hostname, '--port', '22', '--username', imported.username, '--host-key-sha256', pin, ...baseArgs];
async function run(args, home) {
  await mkdir(home, { recursive: true });
  const env = { ...process.env, CODEX_HOME: home, NO_PROXY: 'nanocodex.localhost', no_proxy: 'nanocodex.localhost' };
  delete env.NANOCODEX_AUTH_FILE;
  const child = spawn(binary, args, { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '', err = '';
  child.stdout.on('data', bytes => { out += bytes; }); child.stderr.on('data', bytes => { err += bytes; });
  const timer = setTimeout(() => child.kill('SIGKILL'), 20_000);
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  clearTimeout(timer);
  if (serverFailure) throw serverFailure;
  assert.ok(!out.includes(pem) && !err.includes(pem));
  assert.ok(!out.includes(pem.split('\n')[1]) && !err.includes(pem.split('\n')[1]));
  return { code, out, err };
}
try {
  const home = join(directory, 'home');
  mode = 'setup';
  const setup = await run(['connect', 'github', ...baseArgs], home);
  assert.equal(setup.code, 0, setup.err);
  mode = 'success'; requests = [];
  const success = await run(sshArgs, home);
  assert.equal(success.code, 0, `${success.err} requests=${requests.map(r => r.path).join(",")}`);
  assert.equal(requests.filter(r => r.path === '/v1/connections').length, 1);
  assert.deepEqual(requests.find(r => r.path === '/v1/connections').body.requested_connectors, ['github']);
  const stored = await readFile(join(home, 'connect.json'), 'utf8');
  assert.ok(!stored.includes('PRIVATE KEY') && !stored.includes('ssh_credential_import'));
  assert.ok(JSON.parse(stored).capabilities.includes('github'));
  for (const scenario of ['remote-error', 'ambiguous']) {
    mode = scenario; requests = [];
    const result = await run(sshArgs, join(directory, scenario));
    assert.notEqual(result.code, 0);
    assert.equal(requests.filter(r => r.path === '/v1/connections').length, 1, `${scenario} repeated key POST`);
    assert.match(result.err, /verify its status before starting another import/);
  }
  for (const flag of ['--key-file', '--reference', '--hostname', '--port', '--username', '--host-key-sha256']) {
    requests = [];
    const result = await run(['connect', 'github', flag, flag === '--port' ? '22' : 'unused', ...baseArgs], join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0); assert.match(result.err, /SSH flags require/);
  }
  for (const [index, value] of [[5, 'constructor'], [7, 'localhost'], [7, '192.168.1.1'], [7, 'Host.Example'], [9, '0'], [13, 'SHA256:bad']]) {
    requests = []; const args = [...sshArgs]; args[index] = value;
    // Missing path proves target metadata rejection precedes opening the key.
    args[3] = join(directory, 'missing.pem');
    const result = await run(args, join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0);
    assert.ok(!result.err.includes('could not inspect SSH key file'));
  }
  {
    requests = []; const args = [...sshArgs];
    args[5] = 'r'.repeat(64); args[7] = [63,63,63,61].map(n => 'a'.repeat(n)).join('.'); args[11] = 'u'.repeat(128);
    const result = await run(args, join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0); assert.match(result.err, /512-byte/);
  }
  {
    requests = []; const args = [...sshArgs]; args.splice(2, 0, 'chatgpt');
    const result = await run(args, join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0); assert.match(result.err, /separate connect requests/);
  }
  const invalid = join(directory, 'invalid.pem');
  const link = join(directory, 'link.pem'); await symlink(keyFile, link);
  for (const value of ['private-sentinel-not-a-key', 'x'.repeat(65537), '-----BEGIN OPENSSH PRIVATE KEY-----\nAA==\n-----END OPENSSH PRIVATE KEY-----']) {
    await writeFile(invalid, value); requests = [];
    const args = [...sshArgs]; args[3] = invalid;
    const result = await run(args, join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0);
    assert.ok(!result.err.includes('private-sentinel-not-a-key'));
  }
  for (const path of [link, directory, join(directory, 'missing.pem')]) {
    requests = []; const args = [...sshArgs]; args[3] = path;
    const result = await run(args, join(directory, 'invalid'));
    assert.notEqual(result.code, 0); assert.equal(requests.length, 0);
  }
  console.log('SSH CLI executable HTTP journey passed: exact commitment/body, approval, preserved github grant, no key output/store, no retry on remote/ambiguous failures, rejected flags/files.');
} finally {
  await new Promise(resolve => server.close(resolve));
  await rm(directory, { recursive: true, force: true });
}
