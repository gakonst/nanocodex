// Opt-in contract test against an explicitly selected official Codex binary.
// Only a synthetic MCP server runs: no real CUA, model turn, credentials or GUI.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';

const fixtureFlag = '--synthetic-cua-mcp';
const source = fileURLToPath(import.meta.url);
const approval = {
  mode: 'form',
  message: 'Allow Computer Use to use "Synthetic App"?',
  requestedSchema: { type: 'object', properties: {} },
  _meta: {
    codex_approval_kind: 'mcp_tool_call', connector_id: 'computer-use',
    connector_name: 'Computer Use', persist: ['session'], riskLevel: 'low',
    tool_name: 'get_app_state', tool_params: { app: 'com.example.synthetic' },
    tool_params_display: [{ name: 'app', display_name: 'App', value: 'Synthetic App' }],
  },
};

async function syntheticProvider() {
  let callId;
  const send = message => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', ...message })}\n`);
  for await (const line of readline.createInterface({ input: process.stdin })) {
    const message = JSON.parse(line);
    if (message.id === undefined) continue;
    if (message.id === 'synthetic-approval' && !message.method) {
      send({ id: callId, result: { content: [{ type: 'text', text: JSON.stringify({
        decision: message.result ?? null, error: message.error ?? null,
      }) }] } });
    } else if (message.method === 'initialize') {
      send({ id: message.id, result: { protocolVersion: message.params.protocolVersion,
        capabilities: { tools: {} }, serverInfo: { name: 'synthetic-cua', version: '1' } } });
    } else if (message.method === 'tools/list') {
      send({ id: message.id, result: { tools: [{ name: 'js',
        description: 'Synthetic empty-form approval fixture; performs no computer use.',
        inputSchema: { type: 'object', properties: {} } }] } });
    } else if (message.method === 'tools/call') {
      callId = message.id;
      send({ id: 'synthetic-approval', method: 'elicitation/create', params: approval });
    } else if (message.method === 'ping') {
      send({ id: message.id, result: {} });
    } else {
      send({ id: message.id, error: { code: -32601, message: 'Unknown synthetic method' } });
    }
  }
}

async function upstream(t, binary, sandbox) {
  assert.ok(path.isAbsolute(binary), 'NANOCODEX_TEST_CODEX_BIN must be an absolute binary path');
  const directory = await mkdtemp(path.join(tmpdir(), 'nanocodex-headless-upstream-'));
  let stop;
  t.after(async () => {
    try { await stop?.(); } finally { await rm(directory, { recursive: true, force: true }); }
  });
  const home = path.join(directory, 'home');
  const codexHome = path.join(home, '.codex');
  const workspace = path.join(directory, 'workspace');
  await Promise.all([mkdir(codexHome, { recursive: true }), mkdir(workspace)]);
  // This isolated config never loads the user's CODEX_HOME or account secrets.
  await writeFile(path.join(codexHome, 'config.toml'), [
    'approval_policy = "never"',
    `sandbox_mode = ${JSON.stringify(sandbox)}`,
    '[mcp_servers.cua_repl]',
    `command = ${JSON.stringify(process.execPath)}`,
    `args = [${JSON.stringify(source)}, ${JSON.stringify(fixtureFlag)}]`,
    'startup_timeout_sec = 15',
    '',
  ].join('\n'), { mode: 0o600 });
  const child = spawn(binary, ['app-server'], {
    cwd: workspace,
    env: { HOME: home, CODEX_HOME: codexHome, PATH: '/usr/bin:/bin:/usr/sbin:/sbin', TMPDIR: directory },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const requests = [];
  const pending = new Map();
  let nextId = 0;
  let exited = false;
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', value => { stderr = (stderr + value).slice(-8192); });
  const failPending = error => {
    for (const request of pending.values()) { clearTimeout(request.timer); request.reject(error); }
    pending.clear();
  };
  const stopped = new Promise(resolve => child.once('close', () => { exited = true; resolve(); }));
  child.once('error', error => failPending(error));
  child.once('exit', (code, signal) => failPending(new Error(`Synthetic app-server exited (${code ?? signal}): ${stderr}`)));
  child.stdin.on('error', failPending);
  const lines = readline.createInterface({ input: child.stdout });
  const send = message => child.stdin.write(`${JSON.stringify(message)}\n`);
  lines.on('line', line => {
    let message;
    try { message = JSON.parse(line); } catch { failPending(new Error('Invalid app-server JSON')); return; }
    if (message.method) {
      if (message.id !== undefined) {
        requests.push(message.method);
        // Never supply acceptance. Unexpected client input is an upstream-test failure.
        send({ id: message.id, error: { code: -32601, message: 'No interactive client in headless fixture' } });
      }
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id); clearTimeout(request.timer);
    if (message.error) request.reject(new Error(`${message.error.message}: ${stderr}`));
    else request.resolve(message.result);
  });
  stop = async () => {
    failPending(new Error('Fixture stopped'));
    lines.close();
    child.stdin.end();
    if (!exited) child.kill('SIGTERM');
    const force = setTimeout(() => { if (!exited) child.kill('SIGKILL'); }, 2000);
    try { await stopped; } finally { clearTimeout(force); }
  };
  const request = (method, params) => new Promise((resolve, reject) => {
    const id = ++nextId;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Synthetic app-server ${method} timed out: ${stderr}`));
    }, 20000);
    pending.set(id, { resolve, reject, timer });
    send({ id, method, params });
  });
  await request('initialize', { clientInfo: { name: 'nanocodex_headless_upstream_test', version: '1' },
    capabilities: { experimentalApi: true } });
  send({ method: 'initialized' });
  const catalog = await request('mcpServerStatus/list', { detail: 'toolsAndAuthOnly' });
  const server = catalog.data.find(value => value.name === 'cua_repl');
  assert.ok(server, 'Synthetic provider must be discovered');
  assert.ok(server.toolsError == null, 'Synthetic provider must have no catalog error');
  assert.ok(Object.values(server.tools).some(tool => tool.name === 'js'));
  const thread = await request('thread/start', { ephemeral: true, historyMode: 'paginated', cwd: workspace });
  assert.equal(thread.approvalPolicy, 'never');
  assert.equal(thread.sandbox.type, sandbox === 'danger-full-access' ? 'dangerFullAccess' : 'readOnly');
  const result = await request('mcpServer/tool/call', {
    threadId: thread.thread.id, server: 'cua_repl', tool: 'js', arguments: {},
  });
  assert.notEqual(result.isError, true, 'Synthetic tool must complete');
  const response = JSON.parse(result.content.find(item => item.type === 'text').text);
  assert.equal(response.error, null, 'Official runtime must resolve the form normally');
  assert.deepEqual(requests, [], 'Official runtime must not emit client input requests');
  return response.decision;
}

if (process.argv.includes(fixtureFlag)) {
  await syntheticProvider();
} else {
  const binary = process.env.NANOCODEX_TEST_CODEX_BIN;
  const options = { timeout: 60000, skip: !binary && 'Set NANOCODEX_TEST_CODEX_BIN to opt in to the official-runtime test' };
  test('official headless full-access + never resolves CUA empty form without client approval', options, async t => {
    const decision = await upstream(t, binary, 'danger-full-access');
    assert.equal(decision.action, 'accept');
    assert.deepEqual(decision.content, {});
  });
  test('official headless read-only + never does not accept CUA empty form', options, async t => {
    const decision = await upstream(t, binary, 'read-only');
    assert.equal(decision.action, 'decline');
    assert.ok(decision.content == null);
  });
}
