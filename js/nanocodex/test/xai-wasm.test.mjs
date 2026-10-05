// Real generated Rust/WASM and synthetic Responses transport only; no fake engine.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from '../runtime/durability-store.mjs';

const text = value => ({ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: value }] });
const sse = output => [
  { type: 'response.output_text.delta', item_id: 'synthetic-message', delta: 'native delta' },
  { type: 'response.completed', response: { id: 'synthetic-response', status: 'completed', output,
    usage: { input_tokens: 20, output_tokens: 5, total_tokens: 25, input_tokens_details: { cached_tokens: 3 } } } },
].map(frame => `data: ${JSON.stringify(frame)}\n\n`).join('');
async function fixture(t, respond) {
  const requests = [];
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      requests.push({ body, headers: request.headers });
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const result = await respond(body, requests.length, response);
      if (result !== undefined) response.end(result);
    } catch (error) { response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/xai/journeys/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL(`${t.name.replace(/[^a-zA-Z0-9]+/g, '-').toLowerCase()}.json`, output), JSON.stringify({
      command: 'node --test js/nanocodex/test/xai-wasm.test.mjs', journey: t.name,
      transport: 'actual Rust/WASM to loopback HTTP Responses/Messages fixture', requests: requests.map(({ body }) => body),
    }, null, 2));
  });
  return { endpoint: `http://127.0.0.1:${server.address().port}/v1/responses`, requests };
}
function database() {
  const db = new DatabaseSync(':memory:');
  for (const sql of sqliteDurabilitySchema) db.exec(sql);
  const store = createSqliteDurabilityStore({ transaction(callback) {
    db.exec('BEGIN IMMEDIATE');
    try {
      const value = callback((sql, params = []) => {
        const statement = db.prepare(sql);
        return /^\s*(SELECT|WITH)\b/i.test(sql) ? statement.all(...params).map(row => ({ ...row })) : (statement.run(...params), []);
      });
      db.exec('COMMIT'); return value;
    } catch (error) { db.exec('ROLLBACK'); throw error; }
  } });
  return { db, store };
}
const run = (agent, input, id) => agent.turn.prompt({ input, ...(id === undefined ? {} : { id }) }).result();

for (const target of ['node', 'browser', 'worker', 'host']) {
  test(`actual ${target} Xai WASM tools, events, context, compaction and durable terminal replay`, { timeout: 30_000 }, async t => {
    const { Xai } = await import(`../${target}/index.mjs`);
    const nativeReasoning = { type: 'reasoning', id: 'reasoning-1', encrypted_content: 'opaque-native/+==', summary: [{ type: 'summary_text', text: 'synthetic' }] };
    const { endpoint, requests } = await fixture(t, (body, index) => {
      if (body.input[0]?.content?.startsWith?.('Summarize the supplied conversation')) return sse([text('Earlier effect completed.')]);
      if (index === 1) return sse([nativeReasoning, { type: 'function_call', call_id: 'effect-1', name: 'read_file', arguments: '{"path":"synthetic.txt"}' }]);
      if (index === 2) {
        assert(body.input.some(item => item.type === 'reasoning' && item.encrypted_content === 'opaque-native/+=='));
        assert(body.input.some(item => item.type === 'function_call_output' && item.call_id === 'effect-1' && item.output === 'synthetic receipt'));
      }
      return sse([text('NATIVE_XAI_OK')]);
    });
    const { db, store } = database(); t.after(() => db.close());
    let effects = 0, authCalls = 0;
    const invocations = [];
    const options = { endpoint, model: 'grok-4.6', workspace: '/synthetic', contextWindowTokens: 100_000,
      requestTimeoutMs: 5000, durability: store, durabilityId: `xai-${target}`,
      ...(target === 'node' ? {} : { module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)) }),
      auth: { headers: () => ({ authorization: `Bearer synthetic-${++authCalls}` }) },
      tools: [{ name: 'read_file', description: 'Explicit synthetic read', parameters: { type: 'object' }, handler(input, context) {
        effects++; assert.deepEqual(input, { path: 'synthetic.txt' }); invocations.push(context);
        return { output: 'synthetic receipt', success: true, metadata: { fixture: true }, structuredResult: { read: true } };
      } }],
    };
    let agent = await Xai.create(options);
    const events = []; const watcher = agent.events.watch(); watcher.onEvent(event => events.push(event));
    const first = await run(agent, 'long synthetic context '.repeat(120), 'effect-request');
    assert.equal(first.finalMessage, 'NATIVE_XAI_OK');
    assert.equal((await first.usage()).input_tokens, 40);
    assert.equal(effects, 1); assert.equal(invocations[0].callId, 'effect-1');
    assert.equal(invocations[0].sessionId, `xai-${target}`); assert.ok(invocations[0].turnId);
    assert(invocations[0].signal instanceof AbortSignal);
    const context = await agent.session.context(); assert.equal(context.workspace, '/synthetic');
    assert(JSON.stringify(context.history).includes('opaque-native/+=='));
    assert(events.some(event => event.type === 'run.started' && event.seq >= 1));
    assert(events.some(event => event.type === 'assistant.delta'));
    assert(events.some(event => event.type === 'tool.result'));
    await run(agent, 'keep this latest turn', 'latest');
    await agent.session.compact();
    const compacted = await agent.session.context(); assert(JSON.stringify(compacted.history).includes('Earlier effect completed.'));
    watcher.off(); await agent.session.shutdown(); agent.dispose();
    const before = requests.length, beforeAuth = authCalls;
    agent = await Xai.create({ ...options, auth: { headers() { throw Error('terminal replay must not authenticate'); } } });
    const replayEvents = []; const replayWatcher = agent.events.watch(); replayWatcher.onEvent(event => replayEvents.push(event));
    const replay = await run(agent, 'long synthetic context '.repeat(120), 'effect-request');
    replayWatcher.off();
    assert(replayEvents.some(event => event.type === 'run.completed' && event.payload.replayed === true));
    assert.equal(replayEvents.filter(event => event.type === 'tool.call').length, 0);
    assert.equal(replay.finalMessage, first.finalMessage); assert.deepEqual(await replay.usage(), await first.usage());
    assert.equal(requests.length, before); assert.equal(authCalls, beforeAuth); assert.equal(effects, 1);
    await assert.rejects(run(agent, 'different prompt', 'effect-request'), /conflict|different|identity|reuse/i);
    const persisted = JSON.stringify([...db.prepare('SELECT payload FROM nanocodex_durable_states').all(), ...db.prepare('SELECT value FROM nanocodex_durable_records').all()]);
    assert.doesNotMatch(persisted, /Bearer synthetic-|authorization|terminal replay must not/);
    await agent.session.shutdown(); agent.dispose();
    assert.equal(requests[0].headers.authorization, 'Bearer synthetic-1');
    assert.equal(requests[1].headers.authorization, 'Bearer synthetic-2');
    t.diagnostic(JSON.stringify({ target, requests: requests.length, effects, authCalls, replayRequests: requests.length - before, nativeCompaction: true, events: events.length }));
  });
}

test('normalized host fetch hides its routing capability and a cancelled streaming turn leaves the session reusable', { timeout: 15_000 }, async t => {
  const { Agent } = await import('../node/index.mjs');
  let started; const sampling = new Promise(resolve => { started = resolve; });
  let calls = 0; const observed = [];
  const agent = await Agent.create({ harness: 'xai', model: 'grok-4.6', endpoint: 'https://FIXTURE.invalid:443/v1/responses', requestTimeoutMs: 5000,
    auth: { apiKey: 'synthetic-fetch-secret' }, fetch: async request => {
      observed.push({ url: request.url, headers: Object.fromEntries(request.headers), body: await request.json() });
      assert.equal(request.url, 'https://fixture.invalid/v1/responses');
      assert.equal(request.headers.get('authorization'), 'Bearer synthetic-fetch-secret');
      assert.equal(request.headers.has('x-nanocodex-xai-host'), false);
      if (++calls === 1) {
        started();
        return new Response(new ReadableStream({ start(controller) {
          request.signal.addEventListener('abort', () => controller.error(new Error('cancelled transport')), { once: true });
        } }), { headers: { 'content-type': 'text/event-stream' } });
      }
      return new Response(sse([text('AFTER_CANCEL')]), { headers: { 'content-type': 'text/event-stream' } });
    },
  });
  const turn = agent.turn.prompt({ input: 'wait for cancellation' });
  const result = turn.result(); void result.catch(() => {});
  await sampling; await turn.cancel(); await assert.rejects(result, /cancel/i);
  assert.equal((await run(agent, 'continue')).finalMessage, 'AFTER_CANCEL');
  await agent.session.shutdown(); agent.dispose();
  assert.equal(calls, 2);
  t.diagnostic(JSON.stringify({ actualWasm: true, cancelled: true, resumed: true, fetchRequests: observed.length }));
});

test('invalid auth configuration and thinking are rejected and thrown auth callback secrets are redacted', async () => {
  const { Xai } = await import('../node/index.mjs');
  await assert.rejects(Xai.create({ model: 'grok-4.6', auth: { apiKey: 'secret', headers() {} } }), /exactly one/);
  await assert.rejects(Xai.create({ model: 'grok-4.6', auth: { apiKey: 'secret' }, thinking: 'max' }), /thinking/);
  await assert.rejects(Xai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, maxRetries: -1 }), /maxRetries/);
  await assert.rejects(Xai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, repetitionLimit: 0 }), /repetitionLimit/);
  const agent = await Xai.create({ model: 'grok-4.6', requestTimeoutMs: 1000, auth: { headers() { throw Error('private-token-marker'); } } });
  try { await assert.rejects(run(agent, 'hello'), error => /auth/i.test(String(error)) && !String(error).includes('private-token-marker')); }
  finally { await agent.session.shutdown(); agent.dispose(); }
});

test('xAI steering and rich host outputs retain their native content and event metadata', { timeout: 15_000 }, async t => {
  const { Xai } = await import('../node/index.mjs');
  let started, release;
  const ready = new Promise(resolve => { started = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  const content = [{ type: 'input_text', text: 'RICH_RECEIPT' }, { type: 'input_image', image_url: 'data:image/png;base64,c3ludGhldGlj', detail: 'low' }, { type: 'input_file', file_data: 'data:application/pdf;base64,c3ludGhldGlj', filename: 'fixture.pdf' }];
  const { endpoint, requests } = await fixture(t, (body, index) => index === 1
    ? sse([{ type: 'function_call', call_id: 'rich-call', name: 'read_rich', arguments: '{}' }]) : sse([text('STEERED_RICH_OK')]));
  const agent = await Xai.create({ model: 'grok-4.6', endpoint, auth: { apiKey: 'synthetic' }, requestTimeoutMs: 5000,
    tools: [{ name: 'read_rich', description: 'Explicit synthetic rich data', async handler() { started(); await gate; return { output: content, success: true, metadata: { receipt: 'rich-call' }, structuredResult: { count: 3 } }; } }] });
  const events = []; const watcher = agent.events.watch(); watcher.onEvent(event => events.push(event));
  try {
    const turn = agent.turn.prompt({ input: 'Read the synthetic rich input' });
    await Promise.race([ready, turn.result().then(() => { throw Error('tool was never called'); })]);
    await turn.steer({ input: 'DROP_THIS_STEER', messageId: 'withdraw-me' });
    assert.equal(await turn.withdrawSteer({ messageId: 'withdraw-me' }), true);
    await turn.steer({ input: 'KEEP_THIS_STEER', messageId: 'keep-me' });
    release();
    assert.equal((await turn.result()).finalMessage, 'STEERED_RICH_OK');
    const output = requests[1].body.input.find(item => item.type === 'function_call_output');
    assert.deepEqual(output.output, content);
    const exported = await agent.session.context();
    assert.deepEqual(exported.history.find(item => item.type === 'function_call_output').output, content);
    assert.match(JSON.stringify(requests[1].body.input), /KEEP_THIS_STEER/);
    assert.doesNotMatch(JSON.stringify(requests[1].body.input), /DROP_THIS_STEER/);
    const event = events.find(event => event.type === 'tool.result');
    assert.deepEqual(event.payload.metadata, { receipt: 'rich-call' });
    assert.deepEqual(event.payload.structured_result, { count: 3 });
    t.diagnostic(JSON.stringify({ nativeRichBlocks: output.output.map(item => item.type), steeringRetained: true, pendingSteerWithdrawn: true, toolEventMetadata: event.payload.metadata }));
  } finally { release(); watcher.off(); await agent.session.shutdown(); agent.dispose(); }
});

test('all three WASM families share canonical mixed children with explicit auth and native tools', { timeout: 45_000 }, async t => {
  const { Agent, Subagents } = await import('../node/index.mjs');
  const { Transport } = await import('../host/index.mjs');
  const effects = [], trace = [];
  const { endpoint, requests } = await fixture(t, body => {
    const claude = body.model.startsWith('claude-');
    const history = claude ? body.messages : body.input;
    const encoded = JSON.stringify(history);
    const submitted = history.some(item => item.type === 'function_call' && item.name === 'submit_result' || Array.isArray(item.content) && item.content.some(block => block.type === 'tool_use' && block.name === 'submit_result'));
    const name = !encoded.includes('MIXED_XAI_RECEIPT') ? 'proof' : !submitted ? 'submit_result' : undefined;
    const args = name === 'proof' ? {} : { output: { ok: true, model: body.model } };
    trace.push({ model: body.model, tool: name ?? null });
    if (!claude) return sse(name ? [{ type: 'function_call', call_id: `${name}-${trace.length}`, name, arguments: JSON.stringify(args) }] : [text('CHILD_COMPLETE')]);
    const blocks = name ? [{ type: 'tool_use', id: `${name}-${trace.length}`, name, input: args }] : [{ type: 'text', text: 'CHILD_COMPLETE' }];
    const frames = [{ type: 'message_start', message: { id: 'mixed', role: 'assistant', model: body.model, content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
    blocks.forEach((content_block, index) => frames.push({ type: 'content_block_start', index, content_block }, { type: 'content_block_stop', index }));
    frames.push({ type: 'message_delta', delta: { stop_reason: name ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 1 } }, { type: 'message_stop' });
    return frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join('');
  });
  const proof = { name: 'proof', description: 'Commit one synthetic child effect', parameters: { type: 'object', properties: {} }, handler(_input, context) {
    assert.ok(context.subagent, 'canonical child identity reaches host');
    assert.equal(context.signal.aborted, false);
    effects.push({ model: context.model, agentId: context.subagent.agentId, sessionId: context.sessionId });
    return 'MIXED_XAI_RECEIPT';
  } };
  const capabilities = {
    codex: { model: 'gpt-6.1-sol', thinking: 'low', toolMode: 'direct', tools: [proof], transport: Transport.openAi({ apiKey: 'synthetic-codex', apiBaseUrl: endpoint.replace('/responses', ''), stateless: true }) },
    claude: { model: 'claude-sonnet-4-6', endpoint, auth: { apiKey: 'synthetic-claude' }, tools: [proof] },
    xai: { model: 'grok-4.6', endpoint, auth: { apiKey: 'synthetic-xai' }, requestTimeoutMs: 5000, tools: [proof] },
  };
  for (const family of ['codex', 'claude', 'xai']) {
    const harnesses = Object.fromEntries(Object.entries(capabilities).filter(([name]) => name !== family));
    const root = await Agent.create({ ...capabilities[family], harness: family, ...(family === 'codex' ? {} : { subagents: {} }), harnesses });
    try {
      for (const target of ['codex', 'claude', 'xai'].filter(name => name !== family)) {
        const child = await Subagents.spawn(root, { harness: target, model: capabilities[target].model, role: 'synthetic specialist', task: 'Call proof, then submit_result with ok and your model.', outputSchema: { type: 'object', properties: { ok: { type: 'boolean' }, model: { type: 'string' } }, required: ['ok', 'model'], additionalProperties: false } });
        const report = await Subagents.wait(root, { agentIds: [child.agent_id], timeoutMs: 10_000 });
        assert.equal(report.timed_out, false, JSON.stringify(report));
        assert.equal(report.agents[0].status.state, 'completed', JSON.stringify(report));
        assert.equal(report.agents[0].status.output.ok, true);
        assert.equal(report.agents[0].status.output.model, capabilities[target].model);
        await Subagents.close(root, child.agent_id);
      }
    } finally { await root.session.shutdown(); root.dispose(); }
  }
  assert.equal(effects.length, 6);
  for (const request of requests) {
    const family = request.body.model.startsWith('grok-') ? 'xai' : request.body.model.startsWith('claude-') ? 'claude' : 'codex';
    assert.equal(family === 'claude' ? request.headers['x-api-key'] : request.headers.authorization, family === 'claude' ? 'synthetic-claude' : `Bearer synthetic-${family}`);
  }
  t.diagnostic(JSON.stringify({ mixedDirections: 6, effects, requests: trace }));
});

test('provider body, custom fetch exceptions, and host tool failures do not expose secrets', { timeout: 15_000 }, async t => {
  const { Xai } = await import('../node/index.mjs');
  for (const [name, fetch] of [
    ['provider', async () => new Response('private-provider-marker', { status: 401 })],
    ['fetch', async () => { throw new Error('private-fetch-marker'); }],
  ]) {
    const agent = await Xai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, requestTimeoutMs: 5000, fetch });
    const events = []; const watcher = agent.events.watch(); watcher.onEvent(event => events.push(event));
    try {
      await assert.rejects(run(agent, 'synthetic error'), error => !String(error).includes('private-'));
      assert.doesNotMatch(JSON.stringify(events), /private-provider-marker|private-fetch-marker/);
    } finally { watcher.off(); await agent.session.shutdown(); agent.dispose(); }
    t.diagnostic(JSON.stringify({ failure: name, exposedPrivateMarker: false }));
  }
  const { endpoint, requests } = await fixture(t, (_body, index) => index === 1
    ? sse([{ type: 'function_call', call_id: 'failed-tool', name: 'fails', arguments: '{}' }]) : sse([text('HOST_FAILURE_HANDLED')]));
  const agent = await Xai.create({ model: 'grok-4.6', endpoint, auth: { apiKey: 'synthetic' }, requestTimeoutMs: 5000,
    tools: [{ name: 'fails', description: 'Synthetic failure', handler() { throw new Error('private-tool-marker'); } }] });
  const events = []; const watcher = agent.events.watch(); watcher.onEvent(event => events.push(event));
  try {
    assert.equal((await run(agent, 'synthetic tool failure')).finalMessage, 'HOST_FAILURE_HANDLED');
    const output = requests[1].body.input.find(item => item.type === 'function_call_output');
    assert.equal(output.output, 'Xai tool execution failed');
    assert.doesNotMatch(JSON.stringify(events), /private-tool-marker/);
    t.diagnostic(JSON.stringify({ failure: 'host-tool', providerReceived: output.output, exposedPrivateMarker: false }));
  } finally { watcher.off(); await agent.session.shutdown(); agent.dispose(); }
});

test('durable rejected provider turn replays its failure event without authenticating or sending again', { timeout: 15_000 }, async t => {
  const { Xai } = await import('../node/index.mjs');
  const { db, store } = database(); t.after(() => db.close());
  let requests = 0;
  const options = { model: 'grok-4.6', requestTimeoutMs: 5000, durability: store, durabilityId: 'failed-replay', auth: { apiKey: 'synthetic' },
    fetch: async () => { requests++; return new Response('private-rejection-marker', { status: 401 }); } };
  let agent = await Xai.create(options);
  try { await assert.rejects(run(agent, 'same rejected request', 'failure-id')); }
  finally { await agent.session.shutdown(); agent.dispose(); }
  agent = await Xai.create({ ...options, auth: { headers() { throw new Error('replay must not authenticate'); } } });
  const events = []; const watcher = agent.events.watch(); watcher.onEvent(event => events.push(event));
  try {
    await assert.rejects(run(agent, 'same rejected request', 'failure-id'), error => !String(error).includes('private-rejection-marker'));
    assert.equal(requests, 1);
    assert(events.some(event => event.type === 'run.failed' && event.payload.replayed === true));
    assert.doesNotMatch(JSON.stringify(events), /private-rejection-marker|replay must not authenticate/);
    t.diagnostic(JSON.stringify({ providerRequests: requests, replayRequests: 0, replayFailureEvent: true }));
  } finally { watcher.off(); await agent.session.shutdown(); agent.dispose(); }
});

test('xAI retry policy distinguishes explicit rejections from interrupted streams in actual WASM', { timeout: 15_000 }, async t => {
  const { Xai } = await import('../node/index.mjs');
  const evidence = [];
  for (const maxRetries of [0, 1]) {
    let requests = 0;
    const agent = await Xai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, maxRetries, requestTimeoutMs: 3000,
      fetch: async () => ++requests === 1 ? new Response('unavailable', { status: 503 })
        : new Response(sse([text('RECOVERED')]), { headers: { 'content-type': 'text/event-stream' } }),
    });
    try {
      if (maxRetries === 0) await assert.rejects(run(agent, 'known rejection'), /503/);
      else assert.equal((await run(agent, 'known rejection')).finalMessage, 'RECOVERED');
      assert.equal(requests, maxRetries + 1);
      evidence.push({ maxRetries, requests });
    } finally { await agent.session.shutdown(); agent.dispose(); }
  }
  let requests = 0, effects = 0;
  const agent = await Xai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, maxRetries: 4, requestTimeoutMs: 3000,
    tools: [{ name: 'effect', description: 'Count a synthetic side effect', handler() { effects++; return 'done'; } }],
    fetch: async () => { requests++; return new Response(`data: ${JSON.stringify({ type: 'response.output_item.done', item: { type: 'function_call', call_id: 'interrupted-effect', name: 'effect', arguments: '{}' } })}\n\n`, { headers: { 'content-type': 'text/event-stream' } }); },
  });
  try {
    await assert.rejects(run(agent, 'interrupted response'));
    assert.equal(requests, 1); assert.equal(effects, 0);
    evidence.push({ interruptedRequests: requests, effects });
  } finally { await agent.session.shutdown(); agent.dispose(); }
  const output = new URL('../../../output/xai/journeys/recovery-policy.json', import.meta.url);
  await mkdir(new URL('./', output), { recursive: true });
  await writeFile(output, JSON.stringify(evidence, null, 2));
  t.diagnostic(JSON.stringify(evidence));
});

test('xAI SDK applies tool repetition and compaction tail limits at the native runtime boundary', { timeout: 15_000 }, async t => {
  const { Xai } = await import('../node/index.mjs');
  let effects = 0;
  const { endpoint, requests } = await fixture(t, (body, index) => {
    if (index < 3) return sse([{ type: 'function_call', call_id: `repeat-${index}`, name: 'effect', arguments: '{}' }]);
    if (body.tools?.length === 0) return sse([text('SUMMARY_RETAINED')]);
    return sse([text('BOUNDED_OK')]);
  });
  const agent = await Xai.create({ model: 'grok-4.6', endpoint, auth: { apiKey: 'synthetic' }, requestTimeoutMs: 3000,
    repetitionLimit: 1, compactionKeepTail: 0, instructions: 'CALLER_POLICY_RETAINED',
    tools: [{ name: 'effect', description: 'Count a synthetic side effect', handler() { effects++; return 'EFFECT_RECEIPT'; } }],
  });
  try {
    assert.equal((await run(agent, `EARLIEST_ONLY ${'a'.repeat(1000)}`)).finalMessage, 'BOUNDED_OK');
    assert.equal(effects, 1);
    const receipts = requests[2].body.input.filter(item => item.type === 'function_call_output');
    assert.equal(receipts.length, 2);
    assert.equal(receipts[0].output, 'EFFECT_RECEIPT');
    assert.match(receipts[1].output, /repetition limit/i);
    await run(agent, `MIDDLE_ONLY ${'b'.repeat(1000)}`);
    await run(agent, 'LATEST_RETAINED');
    await agent.session.compact();
    const history = JSON.stringify((await agent.session.context()).history);
    assert.match(history, /SUMMARY_RETAINED/);
    assert.match(history, /CALLER_POLICY_RETAINED/);
    assert.match(history, /LATEST_RETAINED/);
    assert.doesNotMatch(history, /EARLIEST_ONLY|MIDDLE_ONLY|EFFECT_RECEIPT/);
    const summary = requests.at(-1).body;
    assert.equal(summary.tools.length, 0);
    assert.match(JSON.stringify(summary.input), /EARLIEST_ONLY/);
    assert.match(JSON.stringify(summary.input), /MIDDLE_ONLY/);
    t.diagnostic(JSON.stringify({ actualWasm: true, repetitionLimit: 1, effects, compactionKeepTail: 0, retainedLatestTurn: true }));
  } finally { await agent.session.shutdown(); agent.dispose(); }
});
