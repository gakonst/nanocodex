import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createClaude, toClaudeConfig } from '../runtime/claude.mjs';
import { createClaudeHost, resolveClaudeTools } from '../runtime/claude-host.mjs';
import { createMemoryDurabilityStore } from '../runtime/durability-store.mjs';

const MODEL = 'claude-test';
const SESSION = '018f1f9a-7b3c-7a01-8000-000000000031';

test('Claude validates explicit opt-in config and serializes no credentials', () => {
  const auth = { apiKey: 'private-key' };
  const cfg = toClaudeConfig({ auth, model: MODEL, endpoint: 'https://approved.test/messages', compatibilityProfile: 'subscription', cache: '1h', maxTokens: 128 });
  assert.equal(cfg.subscriptionCompatibility, true);
  assert.equal(cfg.cache, '1h');
  assert.equal(JSON.stringify(cfg).includes('private-key'), false);
  for (const value of [undefined, {}, { model: '' }, { model: MODEL, apiKey: 'secret' }, { model: MODEL, thinking: 'extreme' }, { model: MODEL, autoCompact: false }, { model: MODEL, maxTokens: 0 }, { model: MODEL, compatibilityProfile: 'subscription' }, { model: MODEL, endpoint: 'https://key:secret@bad.test' }, { model: MODEL, durabilityId: 'x' }]) {
    assert.throws(() => toClaudeConfig(value), TypeError);
  }
  assert.throws(() => toClaudeConfig({ model: MODEL, endpoint: 'secret' }), (e) => !String(e).includes('secret'));
});

test('public subscription identity is explicit, bounded and never an auth source', () => {
  const input={auth:{headers:()=>({authorization:'Bearer synthetic-only'})},model:MODEL,endpoint:'https://approved.test/messages',compatibilityProfile:'subscription',subscriptionIdentity:{installId:'stable-install',accountUuid:'public-account',platform:'darwin',arch:'arm64',version:'2.1.280'}};
  const cfg=toClaudeConfig(input);
  assert.deepEqual(cfg.subscriptionIdentity,input.subscriptionIdentity);
  for(const identity of [{installId:''},{installId:'bad\nvalue'},{installId:'a'.repeat(8193)},{password:'never-accepted'},{arch:42}]) {
    assert.throws(()=>toClaudeConfig({...input,subscriptionIdentity:identity}),TypeError);
  }
  assert.throws(()=>toClaudeConfig({...input,compatibilityProfile:undefined}),TypeError);
});

test('Claude authentication is instance-scoped, refreshed per request, redacted and released', async () => {
  const left = createClaudeHost({ auth: { apiKey: 'left-secret' } });
  let n = 0;
  const right = createClaudeHost({ auth: { headers: async () => ({ authorization: `Bearer right-${++n}` }) } });
  assert.deepEqual(JSON.parse(await left.claudeAuthHeaders()), { 'x-api-key': 'left-secret' });
  assert.equal(JSON.parse(await right.claudeAuthHeaders()).authorization, 'Bearer right-1');
  assert.equal(JSON.parse(await right.claudeAuthHeaders()).authorization, 'Bearer right-2');
  const failed = createClaudeHost({ auth: { headers() { throw new Error('secret-body'); } } });
  await assert.rejects(failed.claudeAuthHeaders(), (e) => !String(e).includes('secret-body'));
  for (const auth of [undefined, {}, { apiKey: '' }, { apiKey: 'x', headers() {} }]) assert.throws(() => createClaudeHost({ auth }), TypeError);
  left.dispose();
  await assert.rejects(left.claudeAuthHeaders(), /unavailable/);
});

test('Claude-only catalog maps native inputs, errors, media and stable host identities losslessly', async () => {
  assert.deepEqual(resolveClaudeTools().definitions, []);
  assert.throws(() => resolveClaudeTools({}), /explicit array/);
  assert.throws(() => resolveClaudeTools([{ name: 'Read', description: 'x', handler() {}, inputSchema: { type: 'string' } }]), /object schema/);
  assert.throws(() => resolveClaudeTools(Array(2).fill({ name: 'Read', description: 'x', handler() {} })), /duplicate/);
  let context;
  const host = createClaudeHost({ auth: { apiKey: 'secret' }, tools: [
    { name: 'Read', description: 'actual host read', inputSchema: { type: 'object', properties: { file_path: { type: 'string' } } }, handler(input, ctx) {
      assert.deepEqual(input, { file_path: 'a' }); context = ctx;
      return { output: [{ type: 'input_text', text: 'denied' }, { type: 'input_image', image_url: 'data:image/png;base64,aGVsbG8=', detail: 'auto' }], success: false, structuredResult: { code: 'DENIED' }, metadata: { source: 'host' } };
    } },
    { name: 'Bash', description: 'actual host execution', handler() { throw new Error('secret'); } },
  ] });
  assert.deepEqual(JSON.parse(host.toolDefinitions()).map((x) => x.name), ['Read', 'Bash']);
  const out = JSON.parse(await host.executeClaudeTool('Read', '{"file_path":"a"}', SESSION, 'call-1', MODEL, 'turn-1'));
  assert.equal(out.isError, true);
  assert.deepEqual(out.structuredResult, { code: 'DENIED' });
  assert.deepEqual(out.metadata, { source: 'host' });
  assert.deepEqual(out.content[1], { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'aGVsbG8=' } });
  assert.equal(context.sessionId, SESSION); assert.equal(context.turnId, 'turn-1'); assert.equal(context.callId, 'call-1');
  assert.equal(context.model, MODEL); assert.ok(context.signal instanceof AbortSignal);
  const error = JSON.parse(await host.executeClaudeTool('Bash', '{}', SESSION, 'call-2', MODEL, 'turn-1'));
  assert.equal(error.isError, true); assert.equal(JSON.stringify(error).includes('secret'), false);
  await assert.rejects(host.executeTool('Read', '{}', SESSION, 'call', MODEL, undefined), /identities/);
  host.cancelCodeTurn(SESSION); assert.equal(context.signal.aborted, true);
  host.dispose();
});

function mockWasm(capture) {
  return { async create(encoded) {
    const config = JSON.parse(encoded); capture.config = config;
    await globalThis.nanocodexHost.claudeAuth(config.authHostId);
    let freed = 0;
    return capture.raw = {
      sessionId: config.sessionId, agentId: config.sessionId,
      setEventForwarding(enabled) { capture.forwarding = enabled; },
      prompt(input, id) {
        capture.prompt = { input, id };
        return { accepted: async () => id, cancel: async () => {}, free() {}, result: async () => ({
          finalMessage: 'ok', snapshot: () => '{}', usage: () => '{}', free() {},
        }) };
      },
      compact: async () => {}, cancel: async () => {}, shutdown: async () => { capture.shutdown = true; },
      free() { capture.freed = ++freed; },
    };
  } };
}

test('Claude reuses shared prompt/replay, event, durability and graceful cleanup paths', async () => {
  const capture = {};
  const agent = await createClaude({ auth: { apiKey: 'never-in-config' }, model: MODEL, sessionId: SESSION,
    durability: createMemoryDurabilityStore(SESSION), durabilityId: SESSION }, () => mockWasm(capture), 'test');
  assert.equal(capture.config.sessionId, SESSION);
  assert.equal(JSON.stringify(capture.config).includes('never-in-config'), false);
  const bridge = globalThis.nanocodexHost;
  const watcher = agent.events.watch();
  const seen = [];
  watcher.onEvent((event) => seen.push(event));
  assert.equal(capture.forwarding, true);
  bridge.emitEvent(SESSION, JSON.stringify({ request_id: SESSION, type: 'test' }));
  assert.equal(seen.length, 1);
  const turn = agent.turn.prompt({ input: 'hi', id: 'replay-id' });
  assert.equal(await turn.accepted(), 'replay-id');
  assert.equal((await turn.result()).finalMessage, 'ok');
  assert.deepEqual(capture.prompt, { input: 'hi', id: 'replay-id' });
  assert.throws(() => agent.turn.prompt({ input: [] }), /text/);
  await agent.session.compact(); await agent.session.cancel();
  const acquired = await bridge.durabilityAcquire(capture.config.durabilityHostId, SESSION, 'owner');
  assert.equal(acquired.owner_id, 'owner');
  await agent.session.shutdown();
  assert.equal(capture.shutdown, true); assert.equal(capture.freed, 1); assert.equal(capture.forwarding, false);
  assert.throws(() => bridge.claudeAuth(capture.config.authHostId), /definition host/);
  await assert.rejects(bridge.durabilityAcquire(capture.config.durabilityHostId, SESSION, 'owner'), /no Nanocodex host owns/);
  assert.throws(() => bridge.emitEvent(SESSION, '{}'), /no Nanocodex host/);
  const next = await createClaude({ auth: { apiKey: 'x' }, model: MODEL, sessionId: SESSION }, () => mockWasm({}), 'test');
  next.dispose();
});

test('Claude failed construction unregisters auth, durability and session routes', async () => {
  let config;
  await assert.rejects(createClaude({ auth: { apiKey: 'x' }, model: MODEL, sessionId: SESSION,
    durability: createMemoryDurabilityStore(SESSION), durabilityId: SESSION }, () => ({ create(encoded) { config = JSON.parse(encoded); throw new Error('fixture'); } }), 'test'), /fixture/);
  assert.throws(() => globalThis.nanocodexHost.claudeAuth(config.authHostId), /definition host/);
  await assert.rejects(globalThis.nanocodexHost.durabilityAcquire(config.durabilityHostId, SESSION, 'owner'));
  const agent = await createClaude({ auth: { apiKey: 'x' }, model: MODEL, sessionId: SESSION }, () => mockWasm({}), 'test');
  agent.dispose();
});


test('Claude native replies retain opaque media/errors and unsupported shared audio fails closed', async () => {
  const native = { content: [{ type: 'text', text: 'denied', opaque: { retained: true } },
    { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'aA==' } }],
    isError: true, metadata: { evidence: 'host' }, structuredResult: { code: 'NO' } };
  const host = createClaudeHost({ auth: { apiKey: 'x' }, tools: [
    { name: 'Native', description: 'native contract', handler: () => native },
    { name: 'Audio', description: 'unsupported media', handler: () => [{ type: 'input_audio', audio_url: 'data:audio/wav;base64,aA==' }] },
  ] });
  assert.deepEqual(JSON.parse(await host.executeClaudeTool('Native', '{}', SESSION, 'call', MODEL, 'turn')), native);
  await assert.rejects(host.executeClaudeTool('Audio', '{}', SESSION, 'audio-call', MODEL, 'turn'), /unsupported Claude tool media/);
  host.dispose();
});

test('Claude durable identity defaults to durabilityId and rejects explicit drift before construction', async () => {
  const capture = {};
  const store = createMemoryDurabilityStore(SESSION);
  const agent = await createClaude({ auth: { apiKey: 'x' }, model: MODEL, durability: store, durabilityId: SESSION }, () => mockWasm(capture), 'test');
  assert.equal(agent.sessionId, SESSION); assert.equal(capture.config.sessionId, SESSION);
  await agent.session.shutdown();
  assert.throws(() => toClaudeConfig({ auth: { apiKey: 'x' }, model: MODEL, durability: store, durabilityId: SESSION, sessionId: 'different' }), /must equal/);
});

test('Claude tool abort signals are scoped to stable turn identities', async () => {
  const signals = new Map();
  const host = createClaudeHost({ auth: { apiKey: 'x' }, tools: [{ name: 'Effect', description: 'capture signal', handler(_input, context) {
    signals.set(context.turnId, context.signal); return 'ok';
  } }] });
  await host.executeTool('Effect', '{}', SESSION, 'c1', MODEL, 'active');
  await host.executeTool('Effect', '{}', SESSION, 'c2', MODEL, 'queued');
  host.cancelCodeTurn(SESSION, 'queued');
  assert.equal(signals.get('queued').aborted, true);
  assert.equal(signals.get('active').aborted, false);
  host.cancelCodeTurn(SESSION);
  assert.equal(signals.get('active').aborted, true);
  await host.executeTool('Effect', '{}', SESSION, 'c3', MODEL, 'next');
  assert.equal(signals.get('next').aborted, false);
  host.dispose(); assert.equal(signals.get('next').aborted, true);
});


test('Claude refuses explicit Codex definitions rather than reinterpreting their contract', () => {
  for (const name of ['exec', 'wait', 'web__run', 'tool_search', 'exec_command', 'write_stdin', 'apply_patch', 'image_gen__imagegen']) {
    assert.throws(() => resolveClaudeTools([{ name, description: 'must not leak', handler() {} }]), /Codex tool definitions/);
  }
  assert.throws(() => resolveClaudeTools([{ name: 'spawn_agent', description: 'must not override the platform', handler() {} }]), /shared runtime/);
  assert.deepEqual(resolveClaudeTools([{ name: 'Bash', description: 'native explicit host', handler() {} }]).definitions.map(tool => tool.name), ['Bash']);
});

test('Claude snapshots nested configuration before asynchronous loading', async () => {
  const systemBlocks = [{ type: 'text', text: 'original' }];
  const serverTools = [{ type: 'web_search_20250305', name: 'web_search' }];
  const ready = Promise.withResolvers();
  const capture = {};
  const creating = createClaude({ auth: { apiKey: 'synthetic' }, model: MODEL, systemBlocks, serverTools }, async () => {
    await ready.promise; return mockWasm(capture);
  }, 'test');
  systemBlocks[0].text = 'mutated'; serverTools[0].name = 'mutated'; ready.resolve();
  const agent = await creating;
  assert.equal(capture.config.systemBlocks[0].text, 'original');
  assert.equal(capture.config.serverTools[0].name, 'web_search');
  agent.dispose();
});

test('Claude preserves native strict/deferLoading flags and rejects unknown tool fields', () => {
  const base = { name: 'Effect', description: 'explicit', handler() {} };
  for (const alias of ['deferLoading', 'defer_loading']) {
    const definitions = resolveClaudeTools([{ ...base, strict: true, [alias]: false }]).definitions;
    assert.equal(definitions[0].strict, true);
    assert.equal(definitions[0].defer_loading, false);
    assert.equal(Object.isFrozen(definitions[0]), true);
  }
  for (const extra of [{ secretIgnored: true }, { strict: 'true' }, { deferLoading: 1 },
    { deferLoading: false, defer_loading: true }, { inputSchema: {}, parameters: {} }]) {
    assert.throws(() => resolveClaudeTools([{ ...base, ...extra }]), TypeError);
  }
});

test('Claude parallel-safe declarations are scheduling metadata, never wire definitions', () => {
  const base = { description: 'explicit', handler() {} };
  const resolved = resolveClaudeTools([{ ...base, name: 'Read', supportsParallelToolCalls: true },
    { ...base, name: 'Write', supportsParallelToolCalls: false }, { ...base, name: 'Edit' }]);
  assert.deepEqual(resolved.parallelSafe, ['Read']);
  for (const definition of resolved.definitions) assert.equal('supportsParallelToolCalls' in definition, false);
  assert.throws(() => resolveClaudeTools([{ ...base, name: 'Read', supportsParallelToolCalls: 'yes' }]), TypeError);
});

for (const outcome of ['success', 'failure']) {
  test(`Claude detached issued turn ${outcome} proactively defers and releases auth/durability routes`, async () => {
    const completion = Promise.withResolvers();
    const capture = {};
    const store = createMemoryDurabilityStore(SESSION);
    const loaded = mockWasm(capture);
    const agent = await createClaude({ auth: { apiKey: 'synthetic' }, model: MODEL, sessionId: SESSION,
      durability: store, durabilityId: SESSION }, async () => {
      const create = loaded.create;
      return { create: async encoded => {
        const raw = await create(encoded);
        raw.prompt = () => ({ hostTurnId: async () => 'ephemeral-host-id', accepted: async () => undefined,
          result: () => completion.promise, cancel: async () => {}, free() {} });
        return raw;
      } };
    }, 'test');
    agent.turn.prompt({ input: 'issued without caller result waiter' });
    agent.dispose(); agent.dispose();
    const bridge = globalThis.nanocodexHost;
    assert.equal(capture.freed, undefined);
    assert.equal(JSON.parse(await bridge.claudeAuth(capture.config.authHostId))['x-api-key'], 'synthetic');
    await bridge.durabilityAcquire(capture.config.durabilityHostId, SESSION, 'owner');
    if (outcome === 'failure') completion.reject(new Error('synthetic turn failure'));
    else completion.resolve({ finalMessage: 'ok', snapshot: () => '{}', usage: () => '{}', free() {} });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(capture.freed, 1);
    assert.throws(() => bridge.claudeAuth(capture.config.authHostId), /definition host/);
    await assert.rejects(bridge.durabilityAcquire(capture.config.durabilityHostId, SESSION, 'owner'), /no Nanocodex host owns/);
    const next = await createClaude({ auth: { apiKey: 'x' }, model: MODEL, sessionId: SESSION }, () => mockWasm({}), 'test');
    next.dispose();
  });
}
