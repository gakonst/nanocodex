// Public SDK against actual provider transports and Rust WASM. Only providers are synthetic.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, readFile, writeFile, rm, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { WebSocketServer } from 'ws';
import { Agent, Backend } from 'nanocodex/node';

const evidence = new URL('../../../output/backend-api/', import.meta.url);
async function trace(name, records) {
  await mkdir(evidence, { recursive: true });
  await writeFile(new URL(name + '.json', evidence), JSON.stringify(records, null, 2));
}
async function directory(t) {
  const path = await mkdtemp(join(tmpdir(), 'nanocodex-backend-'));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}
function response(socket, index, output) {
  socket.send(JSON.stringify({ type: 'response.completed', response: {
    id: `fixture-${index}`, status: 'completed', output,
    usage: { input_tokens: 10, output_tokens: 2, total_tokens: 12 },
  } }));
}
function final(text) { return [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }]; }
function messages(blocks, stop = 'end_turn') {
  const frames = [{ type: 'message_start', message: { id: 'synthetic', role: 'assistant', model: 'claude-opus-5-5', content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((block, index) => frames.push({ type: 'content_block_start', index, content_block: block }, { type: 'content_block_stop', index }));
  frames.push({ type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 2 } }, { type: 'message_stop' });
  return frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join('');
}

// Factory unions use the same public call at runtime and in backend.test-d.mts.
test('backend factories reject conflicting configuration before provider dispatch', async () => {
  assert.deepEqual(Object.keys(Backend).sort(), ['claude', 'codex']);
  assert.equal(Backend.codex({ apiKey: 'synthetic' }).kind, 'codex');
  assert.equal(JSON.stringify(Backend.claude({ apiKey: 'secret-not-serialized' })), '{"kind":"claude"}');
  for (const bad of [{}, { kind: 'other' }, { kind: 'codex' }, null]) await assert.rejects(Agent.create({ backend: bad }), /Backend/);
  for (const field of ['tools', 'transport', 'auth', 'harness', 'filesystem']) {
    await assert.rejects(Agent.create({ backend: Backend.codex({ apiKey: 'synthetic' }), [field]: {} }), /does not accept/);
  }
  for (const [kind, model] of [['codex', 'claude-opus-5-5'], ['codex', 'sonnet'], ['claude', 'gpt-6-astra'], ['claude', 'luna']]) {
    await assert.rejects(Agent.create({ backend: Backend[kind]({ apiKey: 'synthetic' }), model, workspace: '/intentionally-missing-fixture' }), /model family does not match/);
  }
  assert.throws(() => Backend.claude({ apiKey: '' }), /apiKey/);
  assert.throws(() => Backend.claude({ apiKey: 'synthetic', apiBaseUrl: 'https://example.invalid' }), /unsupported/);
  const { Agent: Browser } = await import('nanocodex/browser');
  const { Agent: Host } = await import('nanocodex/host');
  assert.throws(() => Browser.create({ backend: Backend.codex({ apiKey: 'synthetic' }) }), /nanocodex\/node/);
  await assert.rejects(Host.create({ backend: Backend.claude({ apiKey: 'synthetic' }) }), /nanocodex\/node/);
});

test('Backend.codex default Code Mode reads cwd, runs shell and canonical Rust patch, reports errors, joins shutdown', { timeout: 30_000 }, async t => {
  const path = await directory(t);
  const records = [];
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await new Promise(resolve => server.once('listening', resolve));
  t.after(() => { for (const socket of server.clients) socket.terminate(); return new Promise(resolve => server.close(resolve)); });
  let fixtureError;
  server.on('connection', socket => socket.on('message', raw => {
    try {
      const request = JSON.parse(raw); records.push(request);
      const index = records.length;
      if (index === 1) {
        assert.equal(request.model, 'gpt-6-astra');
        response(socket, index, [{ type: 'custom_tool_call', name: 'exec', call_id: 'shell', input: 'text(await tools.exec_command({cmd:"printf original > fixture.txt",yield_time_ms:1000}));' }]);
      } else if (index === 2) {
        assert.match(JSON.stringify(request), /exit_code/);
        response(socket, index, [{ type: 'custom_tool_call', name: 'exec', call_id: 'patch', input: 'text(await tools.apply_patch("*** Begin Patch\\n*** Update File: fixture.txt\\n@@\\n-original\\n+edited\\n*** End Patch"));' }]);
      } else if (index === 3) {
        assert.match(JSON.stringify(request), /Success|fixture.txt/);
        response(socket, index, [{ type: 'custom_tool_call', name: 'exec', call_id: 'read', input: 'text(await tools.exec_command({cmd:"cat fixture.txt; exit 17",yield_time_ms:1000}));' }]);
      } else if (index === 4) {
        assert.match(JSON.stringify(request), /edited/);
        assert.match(JSON.stringify(request), /17/);
        response(socket, index, final('CODEX_EDIT_CONFIRMED'));
      } else {
        socket.send(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', code: 'invalid_request', message: 'synthetic provider rejection' } }));
      }
    } catch (error) { fixtureError = error; socket.terminate(); }
  }));
  // Exercise the cwd default; restore immediately after awaited creation.
  const previous = process.cwd();
  let agent;
  try {
    process.chdir(path);
    agent = await Agent.create({ backend: Backend.codex({ apiKey: 'synthetic', websocketUrl: `ws://127.0.0.1:${server.address().port}`, apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, websocketWarmup: false }) });
  } finally { process.chdir(previous); }
  t.after(() => agent.session.shutdown());
  const turn = agent.turn.prompt({ input: 'Create and edit fixture.txt using local tools.' });
  const result = await turn.result();
  if (fixtureError) throw fixtureError;
  assert.equal(result.finalMessage, 'CODEX_EDIT_CONFIRMED');
  assert.equal(await readFile(join(path, 'fixture.txt'), 'utf8'), 'edited\n');
  await assert.rejects(agent.turn.prompt({ input: 'Exercise provider rejection.' }).result());
  await agent.session.shutdown();
  assert.throws(() => agent.turn.prompt({ input: 'closed' }), /disposed|shutdown/);
  result.dispose(); turn.dispose();
  await trace('js-codex-transport', records);
});

test('Backend.claude installs native tools, edits real workspace, returns tool errors and closes', { timeout: 30_000 }, async t => {
  const path = await directory(t);
  const records = [];
  let fixtureError;
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); records.push(body);
      const index = records.length;
      if (index > 5) { res.writeHead(400, { 'content-type': 'application/json' }); res.end(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', message: 'synthetic provider rejection' } })); return; }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const calls = [
        ['Write', { file_path: 'fixture.txt', content: 'original\n' }],
        ['Edit', { file_path: 'fixture.txt', old_string: 'original', new_string: 'edited' }],
        ['Bash', { command: 'cat fixture.txt; printf shell >> fixture.txt', timeout: 5000 }],
        ['Read', { file_path: '../outside.txt' }],
      ];
      if (index === 1) {
        assert.equal(body.model, 'claude-opus-5-5');
        const names = body.tools.map(tool => tool.name);
        for (const name of ['Read', 'Write', 'Edit', 'Glob', 'Grep', 'Bash']) assert.ok(names.includes(name), name);
        assert.ok(!names.includes('exec_command'));
        assert.deepEqual(body.tools.find(tool => tool.name === 'Edit').input_schema.required, ['file_path', 'old_string', 'new_string']);
      }
      if (index === 4) assert.match(JSON.stringify(body.messages), /edited/);
      if (index === 5) {
        const latest = body.messages.at(-1).content;
        assert.ok(latest.some(block => block.type === 'tool_result' && block.is_error));
        res.end(messages([{ type: 'text', text: 'CLAUDE_EDIT_CONFIRMED' }]));
      } else {
        const [name, input] = calls[index - 1];
        res.end(messages([{ type: 'tool_use', id: `native-${index}`, name, input }], 'tool_use'));
      }
    } catch (error) { fixtureError = error; res.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
  const agent = await Agent.create({ backend: Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` }), workspace: path });
  t.after(() => agent.session.shutdown());
  const result = await agent.turn.prompt({ input: 'Edit fixture.txt and check an invalid path.' }).result();
  if (fixtureError) throw fixtureError;
  assert.equal(result.finalMessage, 'CLAUDE_EDIT_CONFIRMED');
  assert.equal(await readFile(join(path, 'fixture.txt'), 'utf8'), 'edited\nshell');
  await assert.rejects(agent.turn.prompt({ input: 'Exercise provider rejection.' }).result());
  await agent.session.shutdown();
  assert.throws(() => agent.turn.prompt({ input: 'closed' }), /disposed|shutdown/);
  result.dispose();
  await trace('js-claude-transport', records);
});

for (const kind of ['codex', 'claude']) test(`Backend.${kind} shutdown cancels and joins a running native process`, { timeout: 20_000 }, async t => {
  const path = await directory(t);
  const command = "printf '%s' $$ > live.pid; exec sleep 30";
  let backend;
  if (kind === 'codex') {
    const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
    await new Promise(resolve => server.once('listening', resolve));
    t.after(() => { for (const socket of server.clients) socket.terminate(); return new Promise(resolve => server.close(resolve)); });
    server.on('connection', socket => socket.once('message', () => response(socket, 1, [{
      type: 'custom_tool_call', name: 'exec', call_id: 'long-shell',
      input: `text(await tools.exec_command(${JSON.stringify({ cmd: command, yield_time_ms: 30000 })}));`,
    }])));
    backend = Backend.codex({ apiKey: 'synthetic', websocketUrl: `ws://127.0.0.1:${server.address().port}`, apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, websocketWarmup: false });
  } else {
    const server = createServer(async (req, res) => {
      for await (const _ of req) { /* drain request before replying */ }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end(messages([{ type: 'tool_use', id: 'long-shell', name: 'Bash', input: { command, timeout: 60000 } }], 'tool_use'));
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
    backend = Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` });
  }
  const agent = await Agent.create({ backend, workspace: path });
  t.after(() => agent.session.shutdown());
  const result = agent.turn.prompt({ input: 'Run a command until cancelled.' }).result();
  const settled = result.then(value => ({ value }), error => ({ error }));
  const deadline = Date.now() + 5000;
  let pid;
  while (Date.now() < deadline) {
    try { pid = Number(await readFile(join(path, 'live.pid'), 'utf8')); } catch { /* shell is starting */ }
    if (pid) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.ok(pid, 'model tool must start the real native process');
  process.kill(pid, 0);
  await agent.session.shutdown();
  await settled;
  assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' }, 'shutdown must join termination, not merely request it');
  await trace(`js-${kind}-shutdown`, { startedPid: pid, processAbsentAfterShutdown: true });
});

test('Backend.claude restores canonical tasks and next ID from an exported durable store', { timeout: 30_000 }, async t => {
  const { createMemoryDurabilityStore, exportDurabilityState, importDurabilityState } = await import('nanocodex/durability');
  const path = await directory(t);
  const id = 'backend-task-reopen';
  const records = [];
  let fixtureError;
  const toolResult = body => JSON.parse(body.messages.at(-1).content.find(block => block.type === 'tool_result').content);
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); records.push(body);
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const index = records.length;
      if (index === 1 || index === 4) {
        if (index === 4) assert.equal(toolResult(body).tasks[0].subject, 'Retained task');
        res.end(messages([{ type: 'tool_use', id: `create-${index}`, name: 'TaskCreate', input: { subject: index === 1 ? 'Retained task' : 'Next task', description: 'Canonical durable task fixture' } }], 'tool_use'));
      } else if (index === 3) {
        res.end(messages([{ type: 'tool_use', id: 'list-restored', name: 'TaskList', input: {} }], 'tool_use'));
      } else {
        assert.equal(toolResult(body).task.id, index === 2 ? '1' : '2');
        res.end(messages([{ type: 'text', text: index === 2 ? 'TASK_ONE_SAVED' : 'TASK_TWO_RESTORED' }]));
      }
    } catch (error) { fixtureError = error; res.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
  const backend = Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` });
  const corrupt = createMemoryDurabilityStore(id, { revision: '1', payload: 'invalid-json-fixture' });
  await assert.rejects(Agent.create({ backend, workspace: path, durability: corrupt, durabilityId: id }));
  assert.equal(records.length, 0, 'failed durable creation must not dispatch a provider request');
  let store = createMemoryDurabilityStore(id);
  let agent = await Agent.create({ backend, workspace: path, durability: store, durabilityId: id });
  t.after(() => agent.session.shutdown());
  const first = agent.turn.prompt({ input: 'Create a task.', id: 'create-one' });
  assert.equal((await first.result()).finalMessage, 'TASK_ONE_SAVED');
  await agent.session.shutdown();
  const archive = await exportDurabilityState(store, id);
  await writeFile(join(path, 'durable.json'), JSON.stringify(archive));
  store = createMemoryDurabilityStore(id);
  await importDurabilityState(store, JSON.parse(await readFile(join(path, 'durable.json'), 'utf8')));
  agent = await Agent.create({ backend, workspace: path, durability: store, durabilityId: id });
  const second = agent.turn.prompt({ input: 'List retained tasks, then create another task.', id: 'create-two' });
  assert.equal((await second.result()).finalMessage, 'TASK_TWO_RESTORED');
  if (fixtureError) throw fixtureError;
  await agent.session.shutdown();
  await trace('js-claude-durable-tasks', { requests: records, archive, observedTaskIds: ['1', '2'] });
});

test('Backend.claude WebSearch uses the selected Messages endpoint and returns attributed nested results', { timeout: 20_000 }, async t => {
  const path = await directory(t);
  const records = [];
  let fixtureError;
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); records.push(body);
      assert.equal(req.headers['x-api-key'], 'synthetic');
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      if (records.length === 1) {
        assert.ok(body.tools.some(tool => tool.name === 'WebSearch'));
        res.end(messages([{ type: 'tool_use', id: 'search', name: 'WebSearch', input: { query: 'fixture public docs', allowed_domains: ['example.com'] } }], 'tool_use'));
      } else if (records.length === 2) {
        assert.equal(body.tools.length, 1);
        assert.equal(body.tools[0].name, 'web_search');
        assert.deepEqual(body.tools[0].allowed_domains, ['example.com']);
        assert.match(body.tools[0].type, /^web_search_/);
        res.end(messages([{ type: 'text', text: 'Synthetic sourced result.', citations: [{ type: 'web_search_result_location', url: 'https://example.com/fixture' }] }]));
      } else {
        assert.match(JSON.stringify(body.messages.at(-1)), /Source: https:\/\/example.com\/fixture/);
        res.end(messages([{ type: 'text', text: 'NESTED_SEARCH_CONFIRMED' }]));
      }
    } catch (error) { fixtureError = error; res.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
  const agent = await Agent.create({ backend: Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` }), workspace: path });
  t.after(() => agent.session.shutdown());
  assert.equal((await agent.turn.prompt({ input: 'Search public docs.' }).result()).finalMessage, 'NESTED_SEARCH_CONFIRMED');
  if (fixtureError) throw fixtureError;
  assert.equal(records.length, 3);
  await trace('js-claude-websearch', records);
});

for (const kind of ['codex', 'claude']) test(`Backend.${kind} last handle release preserves an accepted public Actions turn`, { timeout: 15_000 }, async t => {
  const { Actions } = await import('nanocodex/node');
  const path = await directory(t);
  let ready;
  const accepted = new Promise(resolve => { ready = resolve; });
  let continueProvider;
  let backend;
  let requestCount = 0;
  if (kind === 'codex') {
    const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
    await new Promise(resolve => server.once('listening', resolve));
    t.after(() => { for (const socket of server.clients) socket.terminate(); return new Promise(resolve => server.close(resolve)); });
    server.on('connection', socket => socket.on('message', () => {
      if (++requestCount === 1) {
        continueProvider = () => response(socket, 1, [{ type: 'custom_tool_call', name: 'exec', call_id: 'detached', input: 'text(await tools.exec_command({cmd:"printf completed > detached.txt",yield_time_ms:1000}));' }]);
        ready();
      } else response(socket, requestCount, final('DETACHED_COMPLETED'));
    }));
    backend = Backend.codex({ apiKey: 'synthetic', websocketUrl: `ws://127.0.0.1:${server.address().port}`, apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, websocketWarmup: false });
  } else {
    const server = createServer(async (req, res) => {
      for await (const _ of req) { /* drain */ }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      if (++requestCount === 1) {
        continueProvider = () => res.end(messages([{ type: 'tool_use', id: 'detached', name: 'Write', input: { file_path: 'detached.txt', content: 'completed' } }], 'tool_use'));
        ready();
      } else res.end(messages([{ type: 'text', text: 'DETACHED_COMPLETED' }]));
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
    backend = Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` });
  }
  const agent = await Agent.create({ backend, workspace: path });
  const turn = Actions.turn.prompt(agent, { input: 'Finish a local write after release.' });
  await accepted;
  agent.dispose();
  continueProvider();
  const result = await turn.result();
  assert.equal(result.finalMessage, 'DETACHED_COMPLETED');
  assert.equal(await readFile(join(path, 'detached.txt'), 'utf8'), 'completed');
  result.dispose(); turn.dispose();
  await trace(`js-${kind}-last-release`, { requestCount, finalMessage: 'DETACHED_COMPLETED', fileContents: 'completed' });
});

test('Backend.claude Grep applies embedded Rust Unicode, flags, multiline and invalid-regex semantics', { timeout: 20_000 }, async t => {
  const path = await directory(t);
  await writeFile(join(path, 'unicode.txt'), 'αβ\r\nK\r\nMIXED\r\nnext\r\n');
  const calls = [
    { pattern: '\\p{Greek}+', output_mode: 'content' },
    { pattern: 'mixed', '-i': true, output_mode: 'count' },
    { pattern: 'MIXED.*next', multiline: true, output_mode: 'content' },
    { pattern: '\\Aαβ\\z', output_mode: 'content' },
    { pattern: 'k', '-i': true, '-o': true, output_mode: 'content' },
    { pattern: '(?=MIXED)' },
  ];
  const expected = [/unicode.txt:1:αβ/, /unicode.txt:1/, /MIXED[\s\S]*next/, /unicode.txt:1:αβ/, /unicode.txt:2:K/, /Claude tool execution failed/];
  const records = [];
  let fixtureError;
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); records.push(body);
      const index = records.length - 1;
      if (index > 0) {
        const result = body.messages.at(-1).content.find(block => block.type === 'tool_result');
        assert.match(result.content, expected[index - 1]);
        if (index === calls.length) assert.equal(result.is_error, true);
      }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end(index < calls.length
        ? messages([{ type: 'tool_use', id: `grep-${index}`, name: 'Grep', input: calls[index] }], 'tool_use')
        : messages([{ type: 'text', text: 'RUST_REGEX_CONFIRMED' }]));
    } catch (error) { fixtureError = error; res.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); });
  const originalPath = process.env.PATH;
  process.env.PATH = '';
  t.after(() => { if (originalPath === undefined) delete process.env.PATH; else process.env.PATH = originalPath; });
  const agent = await Agent.create({ backend: Backend.claude({ apiKey: 'synthetic', endpoint: `http://127.0.0.1:${server.address().port}/v1/messages` }), workspace: path });
  t.after(() => agent.session.shutdown());
  try {
    assert.equal((await agent.turn.prompt({ input: 'Search this Unicode text.' }).result()).finalMessage, 'RUST_REGEX_CONFIRMED');
  } catch (error) { throw fixtureError ?? error; }
  if (fixtureError) throw fixtureError;
  await trace('js-claude-rust-regex', records);
});

test('Backend.codex failed durable creation releases its session for a corrected retry', { timeout: 10_000 }, async t => {
  const { createMemoryDurabilityStore } = await import('nanocodex/durability');
  const path = await directory(t);
  const id = '019a0000-0000-7000-8000-000000000077';
  const backend = Backend.codex({ apiKey: 'synthetic', websocketUrl: 'ws://127.0.0.1:9', apiBaseUrl: 'http://127.0.0.1:9/v1', websocketWarmup: false });
  const options = { backend, workspace: path, sessionId: id, durabilityId: id };
  const corrupt = createMemoryDurabilityStore(id, { revision: '1', payload: 'invalid-json-fixture' });
  await assert.rejects(Agent.create({ ...options, durability: corrupt }));
  const agent = await Agent.create({ ...options, durability: createMemoryDurabilityStore(id) });
  await agent.session.shutdown();
  await trace('js-codex-failed-create', { invalidStateRejected: true, sameSessionCorrectedRetry: true, shutdown: 'joined' });
});
