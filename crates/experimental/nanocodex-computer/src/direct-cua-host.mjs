// Direct upstream CUA MCP host. No Codex executable, model, thread, auth, or
// general app-server implementation. The signed Sky helper's two policy reads
// describe this host's own application-access policy, not a Codex identity.
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, stat, mkdir, mkdtemp, rm, access } from 'node:fs/promises';
import { constants } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const LIMIT = 64 * 1024 * 1024;
const record = v => v !== null && typeof v === 'object' && !Array.isArray(v);
const hasId = v => typeof v?.id === 'string' || (typeof v?.id === 'number' && Number.isFinite(v.id));
const write = (stream, value) => {
  if (stream.destroyed || stream.writableEnded) return false;
  const bytes = `${JSON.stringify(value)}\n`;
  if (Buffer.byteLength(bytes) + stream.writableLength > LIMIT) throw new Error('CUA output queue too large');
  return stream.write(bytes);
};
const safeFailure = 'Direct CUA transport closed; effects may be uncertain. No input was replayed.';

export function readLines(stream, receive, fail, requireJsonRpc = true) {
  let buffer = Buffer.alloc(0), ended = false;
  stream.on('data', chunk => {
    if (ended) return;
    try {
      buffer = Buffer.concat([buffer, chunk]);
      for (;;) {
        const newline = buffer.indexOf(10);
        if (newline < 0) { if (buffer.length > LIMIT) throw new Error('CUA frame too large'); break; }
        if (newline > LIMIT) throw new Error('CUA frame too large');
        const line = buffer.subarray(0, newline); buffer = buffer.subarray(newline + 1);
        if (!line.length) continue;
        const value = JSON.parse(line.toString('utf8'));
        if (!record(value) || (requireJsonRpc ? value.jsonrpc !== '2.0' : value.jsonrpc !== undefined && value.jsonrpc !== '2.0')) throw new Error('Invalid CUA message');
        receive(value);
      }
    } catch { ended = true; fail(); }
  });
  stream.on('error', fail);
  stream.on('end', () => { if (buffer.length) fail(); });
}

// Conservative compatibility boundary: do not turn existing local/MDM policy
// into unrestricted access. Managed installations require a policy integration;
// until then they fail closed. Never inspect Codex auth or account credentials.
export async function checkManagedPolicy({ home = homedir(), user = process.env.USER, inspect = lstat, follow = stat, run = execute, platform = process.platform, codexHome = process.env.CODEX_HOME } = {}) {
  if (codexHome && !path.isAbsolute(codexHome)) throw new Error('CODEX_HOME policy path must be absolute; direct CUA is disabled.');
  const homes = [path.join(home, '.codex')];
  if (codexHome && path.isAbsolute(codexHome) && !homes.includes(codexHome)) homes.push(codexHome);
  const files = ['/etc/codex/requirements.toml', '/etc/codex/managed_config.toml'];
  for (const directory of homes) {
    files.push(path.join(directory, 'requirements.toml'), path.join(directory, 'managed_config.toml'));
    // An owner's ordinary preferences are superseded by that owner's explicit
    // Nanocodex app-access grant. Never read them or auth data. A configuration
    // owned by somebody else may be enforced; it cannot be ignored.
    try {
      const config = await inspect(path.join(directory, 'config.toml'));
      if (config.uid !== process.getuid() || (config.isSymbolicLink?.() && (await follow(path.join(directory, 'config.toml'))).uid !== process.getuid())) throw new Error('Externally owned computer policy requires integration; direct CUA is disabled.');
    } catch (e) { if (e.code !== 'ENOENT') throw e; }
  }
  if (platform === 'darwin') {
    files.push('/Library/Managed Preferences/com.openai.codex.plist');
    if (user && /^[A-Za-z0-9._-]+$/.test(user)) files.push(`/Library/Managed Preferences/${user}/com.openai.codex.plist`);
  }
  for (const file of files) {
    try { await inspect(file); throw new Error('Managed computer policy requires integration; direct CUA is disabled.'); }
    catch (e) { if (e.code !== 'ENOENT') throw e; }
  }
  if (platform === 'darwin') for (const key of ['config_toml_base64', 'requirements_toml_base64']) {
    try {
      await run('/usr/bin/defaults', ['read', 'com.openai.codex', key], { timeout: 5000, maxBuffer: 4096, env: { PATH: '/usr/bin:/bin', HOME: home, LANG: 'C', LC_ALL: 'C' } });
      throw new Error('Managed computer policy requires integration; direct CUA is disabled.');
    } catch (e) {
      // Missing preference is the sole successful negative probe. Other read
      // failures, oversized values, and unknown output remain fail-closed.
      if (e.code !== 1 || typeof e.stderr !== 'string' || !e.stderr.includes('does not exist')) throw e;
    }
  }
}

export function policyReply(request, allowApps) {
  if (!hasId(request)) return null;
  let result;
  switch (request.method) {
    case 'initialize': result = { userAgent: 'nanocodex-cua-policy-host/1', platformFamily: 'unix', platformOs: 'macos' }; break;
    // Upstream asks for status while initializing its config client. Local CUA
    // has no OpenAI account or token; never inspect or synthesize credentials.
    case 'getAuthStatus': result = { authMethod: null, authToken: null, requiresOpenaiAuth: false }; break;
    case 'configRequirements/read': result = { requirements: { computerUse: { allowLockedComputerUse: false, allowPersistentApproval: false } } }; break;
    case 'config/read': result = { config: { computer_use: { default_app_access: allowApps ? 'allow' : 'deny' } }, origins: {}, layers: null }; break;
    default: return { jsonrpc: '2.0', id: request.id, error: { code: -32601, message: `Unsupported Nanocodex CUA policy request${typeof request.method === 'string' && /^[A-Za-z0-9_/-]{1,128}$/.test(request.method) ? ': ' + request.method : ''}.` } };
  }
  return { jsonrpc: '2.0', id: request.id, result };
}

// Only observed native application-access operations are covered by blanket
// app consent. New/unknown SDK operations must not silently expand that grant.
const APP_ACCESS_TOOLS = new Set(['click', 'drag', 'get_app_state', 'paste',
  'perform_secondary_action', 'press_key', 'scroll', 'select_text', 'set_value', 'type_text']);
export function appConsent(request, allowApps, executing) {
  const p = request.params, meta = p?._meta, schema = p?.requestedSchema;
  const emptyConsent = allowApps && executing && request.method === 'elicitation/create'
    && p?.mode === 'form' && record(schema) && schema.type === 'object'
    && record(schema.properties) && Object.keys(schema.properties).length === 0
    && (!schema.required || (Array.isArray(schema.required) && schema.required.length === 0))
    && meta?.codex_approval_kind === 'mcp_tool_call';
  const nativeAccess = meta?.connector_id === 'computer-use' && APP_ACCESS_TOOLS.has(meta?.tool_name)
    && typeof meta?.tool_params?.app === 'string' && /^[A-Za-z0-9._-]{1,256}$/.test(meta.tool_params.app);
  // These are the upstream's application-access prompts, not page forms.
  // The task's normal authorization still governs actions inside an origin.
  let browserAccess = false;
  if (meta?.connector_id === 'browser-use' && ['access_browser_origin', 'download_browser_files', 'upload_browser_files'].includes(meta?.tool_name)) {
    try { const origin = new URL(meta.tool_params.origin); browserAccess = ['https:', 'http:'].includes(origin.protocol) && !origin.username && !origin.password; } catch {}
  }
  const permitted = emptyConsent && (nativeAccess || browserAccess);
  return { jsonrpc: '2.0', id: request.id, result: permitted ? { action: 'accept', content: {} } : { action: 'decline' } };
}

function absolute(value, label) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || value !== path.resolve(value) || /[\x00-\x1f\x7f]/.test(value)) throw new Error(`${label} must be a normalized absolute path.`);
  return value;
}
export function configuration(env = process.env, platform = process.platform) {
  if (platform !== 'darwin') throw new Error('Direct native CUA host requires macOS.');
  const app = absolute(env.NANOCODEX_CUA_NATIVE_APP, 'NANOCODEX_CUA_NATIVE_APP');
  const resources = path.join(app, 'Contents/Resources');
  return { app, provider: absolute(env.NANOCODEX_CUA_NATIVE_PROVIDER, 'NANOCODEX_CUA_NATIVE_PROVIDER'),
    policyHost: absolute(env.NANOCODEX_CUA_POLICY_HOST, 'NANOCODEX_CUA_POLICY_HOST'),
    state: absolute(env.NANOCODEX_CUA_NATIVE_STATE ?? path.join(homedir(), '.nanocodex/s'), 'NANOCODEX_CUA_NATIVE_STATE'),
    helper: path.join(resources, 'cua_node/lib/node_modules/@oai/sky/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService'),
    allowApps: env.NANOCODEX_CUA_APP_CONSENT === 'allow', env };
}

// The upstream config protocol talks only to our local config responder.
// The generated node-repl launcher starts the kernel directly, never Codex.
// No tokens or inherited NODE_OPTIONS enter model-controlled JavaScript.
function environment(config, socket) {
  const env = { SKY_CUA_SERVICE_NATIVE_PIPE_PATH: socket, NODE_REPL_DISABLE_ANALYTICS: '1',
    CODEX_CLI_PATH: config.policyHost, CODEX_HOME: path.dirname(socket),
    NANOCODEX_CUA_APP_CONSENT: config.allowApps ? 'allow' : 'deny' };
  for (const key of ['PATH', 'HOME', 'USER', 'LOGNAME', 'TMPDIR', 'LANG', 'LC_ALL']) if (config.env[key]) env[key] = config.env[key];
  env.NODE_REPL_UNTRUSTED_ENV_ALLOWLIST = 'SKY_CUA_SERVICE_PATH,SKY_CUA_SERVICE_NATIVE_PIPE_PATH';
  return env;
}

export async function runHost(config, { input = process.stdin, output = process.stdout, spawnChild = spawn, managedCheck = checkManagedPolicy } = {}) {
  await managedCheck();
  // Check every ancestor: don't follow a user-controlled symlink to another
  // owner's state or put the native socket in a public directory.
  for (let current = path.parse(config.state).root, parts = config.state.split(path.sep).filter(Boolean); parts.length;) {
    current = path.join(current, parts.shift());
    await mkdir(current, { mode: 0o700 }).catch(e => { if (e.code !== 'EEXIST') throw e; });
    const s = await lstat(current);
    if (!s.isDirectory() || s.isSymbolicLink() || (s.uid !== process.getuid() && s.uid !== 0) || ((s.mode & 0o022) && !(s.uid === 0 && (s.mode & 0o1000)))) throw new Error('Unsafe direct CUA state path.');
  }
  const state = await lstat(config.state);
  if (state.uid !== process.getuid() || (state.mode & 0o077)) throw new Error('Direct CUA state must be private and owned by the current user.');
  const directory = await mkdtemp(path.join(config.state, 'cua-'));
  const socket = path.join(directory, 'sky.sock');
  if (Buffer.byteLength(socket) > 103) { await rm(directory, { recursive: true }); throw new Error('Direct CUA socket path too long.'); }
  const env = environment(config, socket);
  const child = spawnChild(process.execPath, [fileURLToPath(import.meta.url), '--provider-worker'], { env: { ...env, NANOCODEX_CUA_NATIVE_PROVIDER: config.provider }, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.resume();
  const requests = new Map();
  let native, starting, closed = false, queuedBytes = 0, queuedMessages = 0, tail = Promise.resolve();
  let resolveDone; const done = new Promise(resolve => { resolveDone = resolve; });
  const stop = () => {
    if (closed) return; closed = true;
    for (const id of requests.keys()) { try { write(output, { jsonrpc: '2.0', id, error: { code: -32000, message: safeFailure } }); } catch {} }
    requests.clear();
    // The native watchdog owns its helper group. EOF also fires if this host
    // is SIGKILLed by caller cancellation, so a lost host cannot leave a helper.
    native?.stdin.end();
    for (const p of [child, native]) {
      if (!p?.pid) continue;
      try { process.kill(-p.pid, 'SIGTERM'); } catch (e) { if (e.code !== 'ESRCH') p.kill(); }
    }
    Promise.all([child, native].filter(Boolean).map(p => new Promise(resolve => {
      if (p.exitCode !== null || p.signalCode !== null) return resolve();
      p.once('exit', resolve);
      const timer = setTimeout(() => { try { process.kill(-p.pid, 'SIGKILL'); } catch {} resolve(); }, 5000);
      p.once('exit', () => clearTimeout(timer));
    }))).then(() => rm(directory, { recursive: true, force: true })).finally(resolveDone);
  };
  async function startNative() {
    if (starting) return starting;
    starting = (async () => {
      native = spawnChild(process.execPath, [fileURLToPath(import.meta.url), '--native-worker'], { env: { ...env, NANOCODEX_CUA_NATIVE_APP: config.app, NANOCODEX_CUA_POLICY_HOST: config.policyHost, CODEX_HOME: directory, CODEX_CLI_PATH: config.policyHost, NANOCODEX_CUA_APP_CONSENT: config.allowApps ? 'allow' : 'deny' }, detached: true, stdio: ['pipe', 'ignore', 'pipe'] });
      native.stderr.resume(); native.once('error', stop); native.once('exit', stop);
      const deadline = Date.now() + 120000;
      while (!closed && Date.now() < deadline) {
        try { const s = await lstat(socket); if (s.isSocket() && s.uid === process.getuid()) return; }
        catch (e) { if (e.code !== 'ENOENT') throw e; }
        await new Promise(resolve => setTimeout(resolve, 25));
      }
      throw new Error('Direct native CUA helper did not become ready.');
    })();
    return starting;
  }
  child.once('error', stop); child.once('exit', stop);
  child.stdout.once('end', stop); child.stdout.once('close', stop);
  child.stdin.once('error', stop); output.once('error', stop); output.once('close', stop);
  readLines(child.stdout, message => {
    if (closed) return;
    if (message.method && hasId(message)) {
      if (message.method === 'elicitation/create') write(child.stdin, appConsent(message, config.allowApps, [...requests.values()].some(v => v.method === 'tools/call' && v.tool === 'js' && v.forwarded)));
      else write(child.stdin, { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Unsupported CUA provider request.' } });
      return;
    }
    if (!message.method && hasId(message)) {
      if (!requests.has(message.id)) { stop(); return; }
      requests.delete(message.id);
    }
    write(output, message);
  }, stop);
  readLines(input, message => {
    if (closed) return;
    // Cancellation is handled at admission, not behind helper startup/queued
    // calls. Unsent input must never be dispatched after a cancellation.
    if (message.method === 'notifications/cancelled') {
      const id = message.params?.requestId, entry = requests.get(id);
      if (entry && !entry.forwarded) {
        requests.delete(id);
        write(output, { jsonrpc: '2.0', id, error: { code: -32800, message: 'CUA call cancelled before dispatch; no input was issued.' } });
      }
      write(child.stdin, message);
      return;
    }
    let entry;
    if (hasId(message)) {
      if (requests.has(message.id) || requests.size >= 64) { stop(); return; }
      entry = { method: message.method, tool: message.params?.name, forwarded: false };
      requests.set(message.id, entry);
    }
    const bytes = Buffer.byteLength(JSON.stringify(message));
    queuedBytes += bytes; queuedMessages++;
    if (queuedBytes > LIMIT || queuedMessages > 64) { stop(); return; }
    tail = tail.then(async () => {
      if (closed || (entry && requests.get(message.id) !== entry)) return;
      if (message.method === 'initialize') message = { ...message, params: { ...message.params, capabilities: { ...message.params?.capabilities, elicitation: { form: {} } } } };
      if (message.method === 'tools/call' && message.params?.name === 'js') await startNative();
      if (!closed && (!entry || requests.get(message.id) === entry)) {
        if (entry) entry.forwarded = true;
        write(child.stdin, message);
      }
    }).catch(stop).finally(() => { queuedBytes -= bytes; queuedMessages--; });
  }, stop);
  input.once('end', stop); input.once('close', stop);
  const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
  for (const signal of signals) process.on(signal, stop);
  await done;
  for (const signal of signals) process.off(signal, stop);
}

// Owner-lease watchdog: provider cancellation uses SIGKILL, which a host cannot
// catch. Its closed stdin is observed here, and this worker reaps its own signed
// native helper. The worker adds no runtime dependency; it uses the same Node.
export async function runWorker(env = process.env, kind = 'native') {
  const native = kind === 'native';
  let executable;
  if (native) {
    const app = absolute(env.NANOCODEX_CUA_NATIVE_APP, 'NANOCODEX_CUA_NATIVE_APP');
    absolute(env.NANOCODEX_CUA_POLICY_HOST, 'NANOCODEX_CUA_POLICY_HOST');
    executable = path.join(app, 'Contents/Resources/cua_node/lib/node_modules/@oai/sky/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService');
  } else executable = absolute(env.NANOCODEX_CUA_NATIVE_PROVIDER, 'NANOCODEX_CUA_NATIVE_PROVIDER');
  const child = spawn(executable, [], { env: native ? { ...env, CODEX_CLI_PATH: env.NANOCODEX_CUA_POLICY_HOST } : env,
    detached: true, stdio: native ? ['ignore', 'ignore', 'ignore'] : ['pipe', 'pipe', 'ignore'] });
  let finished = false;
  const stop = () => {
    if (finished) return; finished = true;
    try { if (child.pid) process.kill(-child.pid, 'SIGTERM'); } catch {}
    // Keep this timer alive even if the group leader exits. TERM-ignoring
    // descendants in its owned group must still receive SIGKILL.
    setTimeout(async () => {
      try { if (child.pid) process.kill(-child.pid, 'SIGKILL'); } catch {}
      // Both leases may clean the private session. Catalog-only hosts never
      // start a native worker, so their provider watchdog must also remove it.
      if (env.SKY_CUA_SERVICE_NATIVE_PIPE_PATH) {
        const directory = path.dirname(env.SKY_CUA_SERVICE_NATIVE_PIPE_PATH);
        try {
          const s = await lstat(directory);
          if (/^cua-[A-Za-z0-9]+$/.test(path.basename(directory)) && s.isDirectory() && !s.isSymbolicLink() && s.uid === process.getuid() && !(s.mode & 0o077)) await rm(directory, { recursive: true, force: true });
        } catch {}
      }
      process.exit(0);
    }, 1500);
  };
  child.once('error', stop); child.once('exit', stop);
  if (!native) {
    child.stdin.on('error', stop); child.stdout.once('end', () => { process.stdout.end(); stop(); });
    child.stdout.pipe(process.stdout); process.stdin.pipe(child.stdin);
    process.stdout.on('error', stop);
  }
  process.stdin.resume(); process.stdin.once('end', stop); process.stdin.once('error', stop);
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.once(signal, stop);
}

export const runNativeWorker = env => runWorker(env, 'native');

export async function main(args = process.argv.slice(2), env = process.env) {
  if (args.length === 1 && ['--native-worker', '--provider-worker'].includes(args[0])) { await runWorker(env, args[0] === '--native-worker' ? 'native' : 'provider'); return; }
  if (args[0] === '--policy') {
    if (JSON.stringify(args.slice(1)) !== JSON.stringify(['app-server', '--listen', 'stdio://'])) throw new Error('Unsupported native CUA policy invocation.');
    await checkManagedPolicy();
    readLines(process.stdin, request => { const response = policyReply(request, env.NANOCODEX_CUA_APP_CONSENT === 'allow'); if (response) write(process.stdout, response); }, () => { process.exitCode = 1; process.stdin.destroy(); }, false);
    return;
  }
  if (args.length) throw new Error('Unexpected direct CUA host argument.');
  const config = configuration(env);
  for (const executable of [config.provider, config.policyHost, config.helper]) await access(executable, constants.X_OK);
  await runHost(config);
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main().catch(() => {
  console.error('Direct CUA host failed; no action was retried. Check installation, policy, and native permissions.'); process.exitCode = 1;
});
