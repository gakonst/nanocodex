import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { DatabaseSync } from 'node:sqlite';
import test from 'node:test';
import { Agent, Transport, RequestPolicy } from 'nanocodex/node';
import { Agent as HostAgent, Transport as HostTransport } from 'nanocodex/host';
import { Agent as BrowserAgent, Transport as BrowserTransport } from 'nanocodex/browser';
import { bindAgent } from '../cloudflare/Agent.mjs';
import { createMemoryDurabilityStore } from 'nanocodex/durability';

const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
const CloudflareAgent = bindAgent(module);
const gptModels = ['gpt-6.1-sol', 'gpt-6-luna'].map(model => ({ model, family: 'codex', contextTokens: 100_000, maxOutputTokens: 128, switchGroup: 'synthetic-compatible' }));
const usage = { input_tokens: 20, output_tokens: 3, total_tokens: 23 };
function oai(output, id) {
  return new Response([
    ...output.map((item, output_index) => ({ type: 'response.output_item.done', output_index, item })),
    { type: 'response.completed', response: { id, status: 'completed', output, usage } },
  ].map(value => `data: ${JSON.stringify(value)}\n\n`).join(''), { headers: { 'content-type': 'text/event-stream' } });
}
function final(text, id) { return oai([{ type: 'message', id: `${id}-msg`, role: 'assistant', content: [{ type: 'output_text', text }] }], id); }
function claude(blocks, stop = 'end_turn') {
  const frames = [{ type: 'message_start', message: { id: 'synthetic', role: 'assistant', model: 'claude-sonnet-4-6', content: [], usage: { input_tokens: 20, output_tokens: 0, cache_read_input_tokens: 8 } } },
    ...blocks.flatMap((content_block, index) => [{ type: 'content_block_start', index, content_block }, { type: 'content_block_stop', index }]),
    { type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 3 } }, { type: 'message_stop' }];
  return new Response(frames.map(value => `event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`).join(''), { headers: { 'content-type': 'text/event-stream' } });
}
async function server(t, respond) {
  const requests = []; const sockets = new Set();
  const fixture = createServer(async (req, res) => {
    try {
      const chunks = []; for await (const chunk of req) chunks.push(chunk);
      const record = { path: req.url, headers: req.headers, body: JSON.parse(Buffer.concat(chunks)) };
      requests.push(record);
      const response = await respond(record, requests.length);
      res.writeHead(response.status, Object.fromEntries(response.headers));
      if (response.body) for await (const chunk of response.body) res.write(chunk);
      res.end();
    } catch (error) { res.destroy(error); }
  });
  fixture.on('connection', socket => { sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
  await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve));
  t.after(async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => fixture.close(resolve)); });
  return { base: `http://127.0.0.1:${fixture.address().port}/v1`, requests };
}
function policyOptions(durability, durabilityId, extra = {}) {
  return { durability, durabilityId, selection: 'synthetic-balanced', models: gptModels,
    estimateInputTokens: () => 512, switchSafe: () => true,
    route: ({ state }) => ({ model: state === null ? 'gpt-6-luna' : 'gpt-6.1-sol', state: (state ?? 0) + 1 }), ...extra };
}
function evidence(name, value) { console.log(`REQUEST_POLICY_EVIDENCE ${name} ${JSON.stringify(value)}`); }

test('public Node Agent routes physical model, pins tool continuation, configures boundaries and restores router state', { timeout: 30_000 }, async t => {
  let effects = 0;
  const fixture = await server(t, ({ body }, n) => {
    if (n === 1) {
      const tool = (body.input.find(item => item.type === 'additional_tools')?.tools ?? body.tools).find(tool => tool.name === 'effect');
      assert.ok(tool, JSON.stringify(body.tools));
      return oai([{ type: 'function_call', id: 'call-item', call_id: 'effect-call', name: tool.name, arguments: '{}' }], 'first');
    }
    if (n === 2) assert.ok(body.input.some(item => item.type === 'function_call_output' && JSON.stringify(item.output).includes('effect receipt')));
    return final(`OAI_POLICY_OK_${n}`, `response-${n}`);
  });
  const durability = createMemoryDurabilityStore('oai-policy');
  const options = policyOptions(durability, 'oai-policy');
  let policy = await RequestPolicy.create(options);
  await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'PROJECT_V1' } }]);
  let agent = await Agent.create({ model: 'gpt-6.1-sol', thinking: 'low', requestPolicy: policy,
    transport: Transport.openAi({ apiKey: 'synthetic-first', apiBaseUrl: fixture.base }),
    toolMode: 'direct', tools: { effect: { description: 'Return one effect receipt', parameters: { type: 'object' }, handler() { effects++; return 'effect receipt'; } } } });
  try {
    assert.equal((await agent.turn.prompt({ input: 'Call effect then finish' }).result()).finalMessage, 'OAI_POLICY_OK_2');
    assert.deepEqual(fixture.requests.map(r => r.body.model), ['gpt-6-luna', 'gpt-6-luna']);
    assert.equal(effects, 1);
    assert.match(JSON.stringify(fixture.requests[0].body), /PROJECT_V1/);
    assert.equal(fixture.requests[1].body.previous_response_id, undefined);
    await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'PROJECT_V2' } }]);
    assert.equal((await agent.turn.prompt({ input: 'Next boundary' }).result()).finalMessage, 'OAI_POLICY_OK_3');
    assert.equal(fixture.requests[2].body.model, 'gpt-6.1-sol');
    assert.match(JSON.stringify(fixture.requests[2].body), /PROJECT_V2/);
    assert.doesNotMatch(JSON.stringify(fixture.requests[2].body), /PROJECT_V1/);
  } finally { await agent.session.shutdown(); }
  const before = await policy.snapshot();
  policy = await RequestPolicy.create(options);
  agent = await Agent.create({ model: 'gpt-6.1-sol', thinking: 'low', requestPolicy: policy,
    transport: Transport.openAi({ apiKey: 'synthetic-refreshed', apiBaseUrl: fixture.base }), tools: {}, toolMode: 'direct' });
  try {
    assert.equal((await agent.turn.prompt({ input: 'After cold policy recreation' }).result()).finalMessage, 'OAI_POLICY_OK_4');
    const snapshot = await policy.snapshot();
    assert.deepEqual(snapshot.requests.slice(0, 3).map(r => r.original), before.requests.map(r => r.original));
    assert.equal(snapshot.routerState, 3);
    assert.equal(fixture.requests[3].headers.authorization, 'Bearer synthetic-refreshed');
    assert.equal(JSON.stringify(snapshot).includes('synthetic-refreshed'), false);
    assert.ok(snapshot.requests.every(r => r.status === 'completed' && r.usage.output_tokens === 3));
    evidence('node-oai', { models: fixture.requests.map(r => r.body.model), selected: snapshot.selection, routeState: snapshot.routerState, statuses: snapshot.requests.map(r => r.status), effectExecutions: effects, boundaries: snapshot.history });
  } finally { await agent.session.shutdown(); }
});

test('host-managed public Agent rejects changed native tool schemas and unknown model dispatch before transport', { timeout: 30_000 }, async () => {
  let dispatched = 0;
  const policy = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('host-policy'), 'host-policy'));
  const transport = HostTransport.hostManaged({ apiBaseUrl: 'https://synthetic.invalid/v1', stateless: true,
    createWebSocket() { assert.fail('governed requests must use HTTP'); },
    createResponse(_url, _id, request) { dispatched++; assert.equal(request.authorization, 'host_managed'); return final('HOST_POLICY_OK', `host-${dispatched}`); } });
  const agent = await HostAgent.create({ module, model: 'gpt-6.1-sol', thinking: 'low', requestPolicy: policy, transport, toolMode: 'direct', tools: {} });
  try {
    assert.equal((await agent.turn.prompt({ input: 'First request' }).result()).finalMessage, 'HOST_POLICY_OK');
    await policy.configure([{ kind: 'set_tool', tool: { name: 'unauthorized', definition: { type: 'function', name: 'unauthorized', description: 'injected declaration', parameters: { type: 'object' } } } }]);
    await assert.rejects(agent.turn.prompt({ input: 'Denied declaration' }).result(), /unauthorized tool declaration/);
    assert.equal(dispatched, 1);
    evidence('host-authorization', { dispatched, denied: 'configuration includes an unauthorized tool declaration' });
  } finally { await agent.session.shutdown(); }
  const invalid = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('invalid-route'), 'invalid-route', { route: () => ({ model: 'unapproved', state: null }) }));
  const denied = await HostAgent.create({ module, model: 'gpt-6.1-sol', thinking: 'low', requestPolicy: invalid, transport, tools: {}, toolMode: 'direct' });
  try { await assert.rejects(denied.turn.prompt({ input: 'Invalid route' }).result(), /unapproved physical model/); assert.equal(dispatched, 1); }
  finally { await denied.session.shutdown(); }
});

test('public Claude Agent preserves signed tool history and accounts for opt-in cache warm actual usage', { timeout: 30_000 }, async t => {
  const signed = { type: 'thinking', thinking: 'synthetic', signature: 'opaque-signature/+==' };
  let effects = 0; let modelCalls = 0; let headers = 0;
  const fixture = await server(t, ({ body }) => {
    if (body.stream === false) {
      assert.deepEqual(body.tool_choice, { type: 'none' });
      return Response.json({ usage: { input_tokens: 2, output_tokens: 1, cache_creation_input_tokens: 100, cache_read_input_tokens: 0 } });
    }
    modelCalls++;
    if (modelCalls === 1) return claude([signed, { type: 'tool_use', id: 'read-once', name: 'Read', input: {} }], 'tool_use');
    assert.deepEqual(body.messages.find(m => m.role === 'assistant').content[0], signed);
    assert.ok(body.messages.at(-1).content.some(block => block.type === 'tool_result'));
    return claude([{ type: 'text', text: 'CLAUDE_POLICY_OK' }]);
  });
  const policy = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('claude-policy'), 'claude-policy', {
    models: [{ model: 'claude-sonnet-4-6', family: 'claude', contextTokens: 100_000, maxOutputTokens: 128 }],
    route: () => ({ model: 'claude-sonnet-4-6', state: { routed: true } }),
    cacheWarm: { enabled: true, ttlSeconds: 300, maxSpendUsd: .01, estimatedWriteUsd: .001, estimatedReadUsd: .0001, estimatedUncachedUsd: .002,
      reuseProbability: 1, expectedReuseCount: 2, expectedReuseWithinSeconds: 60,
      inputUsdPerMillion: 1, outputUsdPerMillion: 5, cacheWriteUsdPerMillion: 2, cacheReadUsdPerMillion: .1 },
  }));
  await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'CLAUDE_PROJECT' } }]);
  const agent = await Agent.create({ harness: 'claude', model: 'claude-sonnet-4-6', maxTokens: 128, cache: '5m', adaptiveThinking: false,
    endpoint: `${fixture.base}/messages`, auth: { headers: () => ({ 'x-api-key': `synthetic-${++headers}` }) }, requestPolicy: policy,
    tools: [{ name: 'Read', description: 'read fixture', handler() { effects++; return 'read receipt'; } }] });
  try {
    assert.equal((await agent.turn.prompt({ input: 'Read then finish' }).result()).finalMessage, 'CLAUDE_POLICY_OK');
    const snapshot = await policy.snapshot();
    assert.equal(effects, 1); assert.equal(snapshot.requests.length, 2);
    assert.equal(snapshot.requests[1].context.continuationOf, snapshot.requests[0].requestId);
    assert.equal(snapshot.warms.length, 1);
    assert.ok(snapshot.warms.every(w => w.status === 'completed' && Math.abs(w.actualUsd - .000207) < 1e-12));
    assert.ok(Math.abs(snapshot.actualWarmUsd - .000207) < 1e-12);
    assert.ok(fixture.requests[0].body.system.some(block => block.text === 'CLAUDE_PROJECT'));
    assert.deepEqual(snapshot.requests.map(r => r.usage.output_tokens), [3, 3]);
    assert.equal(JSON.stringify(snapshot).includes('x-api-key'), false);
    evidence('node-claude', { wireRequests: fixture.requests.length, modelCalls, effectExecutions: effects, warmReceipts: snapshot.warms, actualWarmUsd: snapshot.actualWarmUsd, statuses: snapshot.requests.map(r => r.status), authResolutions: headers });
  } finally { await agent.session.shutdown(); }
});

function storage(t) {
  const db = new DatabaseSync(':memory:'); t.after(() => db.close());
  return { sql: { exec(sql, ...args) {
    const prepared = db.prepare(sql); const rows = prepared.all(...args);
    return { toArray: () => rows, rowsWritten: 0, [Symbol.iterator]: () => rows[Symbol.iterator]() };
  } }, transactionSync(callback) { db.exec('BEGIN'); try { const value = callback(); db.exec('COMMIT'); return value; } catch (error) { db.exec('ROLLBACK'); throw error; } } };
}
test('Cloudflare public durable Agent applies named policy at the subject-scoped egress boundary', { timeout: 30_000 }, async t => {
  const requests = [];
  const owner = { ctx: { acceptWebSocket() {}, getWebSockets() { return []; }, id: { toString: () => 'a'.repeat(64) }, storage: storage(t) }, env: { NANOCODEX: { async fetch(url, init) {
    requests.push({ url: String(url), headers: Object.fromEntries(init.headers), body: JSON.parse(init.body) });
    return final('CLOUDFLARE_POLICY_OK', 'cloudflare-response');
  } } } };
  const policy = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('cloudflare-policy'), 'cloudflare-policy'));
  await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'HOSTED_PROJECT' } }]);
  const agent = await CloudflareAgent.create(owner, { eventPersistence: 'caller', requestPolicy: policy, tools: {} });
  try {
    assert.equal((await agent.turn.prompt({ input: 'Hosted request' }).result()).finalMessage, 'CLOUDFLARE_POLICY_OK');
    assert.equal(requests.length, 1);
    assert.equal(requests[0].body.model, 'gpt-6-luna');
    assert.match(JSON.stringify(requests[0].body), /HOSTED_PROJECT/);
    assert.equal(requests[0].headers.authorization, 'Bearer NANOCODEX_PROVIDER_CREDENTIAL');
    assert.equal(requests[0].headers['x-nanocodex-subject'], 'a'.repeat(64));
    evidence('cloudflare', { wireRequests: requests.length, model: requests[0].body.model, subject: requests[0].headers['x-nanocodex-subject'], status: (await policy.snapshot()).requests[0].status });
  } finally { await agent.session.shutdown(); }
});


test('public Node forks expose separate policy handles and immutable inherited history', { timeout: 30_000 }, async t => {
  const fixture = await server(t, (_record, n) => final(`FORK_OK_${n}`, `fork-${n}`));
  const policy = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('fork-policy'), 'fork-policy'));
  await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'PARENT_POLICY' } }]);
  const parent = await Agent.create({ model: 'gpt-6.1-sol', thinking: 'low', requestPolicy: policy,
    transport: Transport.openAi({ apiKey: 'synthetic-fork', apiBaseUrl: fixture.base }), tools: {}, toolMode: 'direct' });
  let child;
  try {
    await parent.turn.prompt({ input: 'Parent before fork' }).result();
    const inherited = await policy.snapshot();
    child = await parent.session.fork();
    assert.notEqual(child.requestPolicy, parent.requestPolicy);
    assert.deepEqual((await child.requestPolicy.snapshot()).requests, inherited.requests);
    await child.requestPolicy.configure([{ kind: 'set_section', section: { name: 'project', text: 'CHILD_POLICY' } }]);
    await child.turn.prompt({ input: 'Child after fork' }).result();
    await parent.turn.prompt({ input: 'Parent after fork' }).result();
    const parentState = await parent.requestPolicy.snapshot();
    const childState = await child.requestPolicy.snapshot();
    assert.equal(parentState.routerState, 2);
    assert.equal(childState.routerState, 2);
    assert.deepEqual(childState.requests[0].original, parentState.requests[0].original);
    assert.equal(parentState.configuration.sections[0].text, 'PARENT_POLICY');
    assert.equal(childState.configuration.sections[0].text, 'CHILD_POLICY');
    assert.match(JSON.stringify(fixture.requests[1].body), /CHILD_POLICY/);
    assert.match(JSON.stringify(fixture.requests[2].body), /PARENT_POLICY/);
    assert.doesNotMatch(JSON.stringify(fixture.requests[2].body), /CHILD_POLICY/);
    assert.notEqual(childState.requests[1].requestId, parentState.requests[1].requestId);
    evidence('node-fork', { parentRouterState: parentState.routerState, childRouterState: childState.routerState,
      parentRequest: parentState.requests[1].requestId, childRequest: childState.requests[1].requestId, inheritedOriginal: 'immutable', handles: 'separate' });
  } finally { await child?.session.shutdown(); await parent.session.shutdown(); }
});

test('public browser and Node managed transports deny client policy before harness routing', async () => {
  const policy = await RequestPolicy.create(policyOptions(createMemoryDurabilityStore('managed-policy'), 'managed-policy'));
  for (const [agent, transport] of [[BrowserAgent, BrowserTransport], [Agent, Transport]]) {
    const managed = transport.managed({ agent: { create: true }, apiKey: 'ncx_live_synthetic', baseUrl: 'https://synthetic.invalid',
      fetch() { assert.fail('client policy must be rejected before account transport'); } });
    for (const harness of ['codex', 'claude']) {
      assert.throws(() => agent.create({ transport: managed, requestPolicy: policy, harness }), /owning host/);
    }
  }
  evidence('managed-guard', { families: ['codex', 'claude'], surfaces: ['browser', 'node'], dispatched: 0 });
});

test('public host HTTPS rejects redirects without following provider credentials', { timeout: 20_000 }, async t => {
  const fixture = await server(t, ({ path }, count) => {
    assert.equal(path, '/v1/responses');
    return count === 1 ? final('HOST_HTTPS_OK', 'host-https')
      : new Response(null, { status: 307, headers: { location: `${fixture.base}/redirect-target` } });
  });
  const agent = await HostAgent.create({ module, model: 'gpt-6.1-sol', thinking: 'low',
    transport: HostTransport.hostManaged({ apiBaseUrl: fixture.base, stateless: true,
      createResponse: (endpoint, _sessionId, request) => fetch(endpoint, { method: 'POST',
        headers: { authorization: 'Bearer synthetic-redirect', 'content-type': 'application/json' },
        body: request.body, signal: request.signal, redirect: 'manual' }) }),
    tools: {}, toolMode: 'direct' });
  try {
    assert.equal((await agent.turn.prompt({ input: 'Normal HTTPS' }).result()).finalMessage, 'HOST_HTTPS_OK');
    await assert.rejects(agent.turn.prompt({ input: 'Reject redirect' }).result(), /redirects are not allowed/);
    assert.ok(fixture.requests.length >= 2);
    assert.ok(fixture.requests.every(request => request.path === '/v1/responses'));
    evidence('host-redirect', { requests: fixture.requests.length, followed: 0, credentials: 'synthetic', policy: 'manual and reject' });
  } finally { await agent.session.shutdown(); }
});
