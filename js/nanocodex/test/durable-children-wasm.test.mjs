// Reproduce after pnpm --filter nanocodex-vite build:wasm:
// node --test js/nanocodex/test/durable-children-wasm.test.mjs
// Only external model Responses are synthetic. Every child operation runs in
// the shipped public SDK, generated Rust WASM, and a real SQLite state store.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { Worker } from 'node:worker_threads';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const schema = { type: 'object', properties: { stage: { type: 'string' } }, required: ['stage'], additionalProperties: false };
const task = (marker, lifetime = 'foreground') => ({ role: marker, task: marker + ': perform proof once and submit the typed result.', outputSchema: schema, lifetime });
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
async function within(promise, label, timeout = 15_000) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Timed out: ' + label)), timeout); })]); }
  finally { clearTimeout(timer); }
}

async function fixture(t, label) {
  const directory = await mkdtemp(join(tmpdir(), 'nanocodex-durable-children-'));
  const trace = [], owners = [], requests = [], effects = [], waiting = [], held = [];
  let phase = 'prepare', serial = 0, cleaned = false;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const history = body.input.filter(item => item.type !== 'additional_tools');
      const encoded = JSON.stringify(history);
      const marker = ['ROOT_SPAWN', 'COMPLETED', 'RUNNING', 'INTERRUPTED', 'CLOSED', 'FOREGROUND', 'BACKGROUND', 'DEDUP_CHILD'].find(value => encoded.includes(value));
      const definitions = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      const submissions = history.filter(item => item.type === 'function_call' && item.name === 'submit_result');
      const submitted = submissions.length > 0;
      const mailboxSubmitted = submissions.some(item => JSON.parse(item.arguments).output?.stage === 'mailbox');
      const row = { type: 'model-request', phase, auth: request.headers.authorization, marker, body };
      trace.push(row); requests.push(row); wake();
      response.on('close', () => { row.closed = true; wake(); });
      assert.ok(marker, 'model request belongs to a declared synthetic task');
      let tool, args;
      if (marker === 'ROOT_SPAWN') {
        const spawned = history.some(item => item.type === 'function_call_output' && item.call_id === 'spawn-once');
        if (!spawned) {
          tool = 'spawn_agent';
          args = { role: 'DEDUP_CHILD', task: 'DEDUP_CHILD: perform proof once and submit the typed result.', harness: null, model: null, thinking: null, lifetime: 'foreground', output_contract: { kind: 'object', fields: [{ name: 'stage', schema: { kind: 'string' }, required: true }] } };
        }
      } else if (!encoded.includes('DURABLE_CHILD_EFFECT_RECEIPT')) {
        tool = 'proof'; args = {};
      } else if (phase === 'prepare' && ['RUNNING', 'INTERRUPTED', 'CLOSED', 'FOREGROUND', 'BACKGROUND'].includes(marker)) {
        held.push(() => {
          const boundary = definitions.find(item => item.name === 'boundary');
          assert.ok(boundary, 'synthetic pure boundary tool is available');
          response.writeHead(200, { 'content-type': 'text/event-stream' });
          response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `boundary-${++serial}`, status: 'completed', output: [{ type: 'function_call', call_id: `boundary-${++serial}`, name: boundary.name, arguments: '{}' }], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
        }); return;
      } else if (!submitted || encoded.includes('MAILBOX_AFTER_RESTART') && !mailboxSubmitted) {
        tool = 'submit_result'; args = { output: { stage: encoded.includes('MAILBOX_AFTER_RESTART') ? 'mailbox' : marker.toLowerCase() } };
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const definition = tool && definitions.find(item => item.name === tool || item.description?.startsWith(tool + '\n'));
      if (tool) assert.ok(definition, tool + ' is exposed by the actual runtime');
      const callId = tool === 'spawn_agent' ? 'spawn-once' : `${tool}-${++serial}`;
      const output = tool ? [{ type: 'function_call', call_id: callId, name: definition.name, arguments: JSON.stringify(args) }]
        : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: marker === 'ROOT_SPAWN' ? 'ROOT_COMPLETE' : 'CHILD_COMPLETE' }] }];
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `fixture-${++serial}`, status: 'completed', output, usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } })}\n\n`);
    } catch (error) { trace.push({ type: 'fixture-error', error: String(error) }); response.destroy(error); wake(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  function wake() { for (const item of [...waiting]) if (item.predicate()) { waiting.splice(waiting.indexOf(item), 1); item.resolve(); } }
  async function until(predicate, description) {
    if (predicate()) return;
    const gate = deferred(); waiting.push({ predicate, resolve: gate.resolve });
    await within(gate.promise, description);
  }
  async function start(options = {}) {
    const owner = owners.length + 1, ready = deferred(), pending = new Map(); let next = 0;
    const worker = new Worker(new URL('./support/durable-children.worker.mjs', import.meta.url), { workerData: {
      databasePath: join(directory, 'durability.sqlite'), baseUrl: `http://127.0.0.1:${server.address().port}/v1`, auth: `synthetic-owner-${owner}`, ...options,
    } });
    const api = { owner, worker, call(action, args = {}) {
      const id = ++next, gate = deferred(); pending.set(id, gate);
      worker.postMessage({ id, action, args }); return within(gate.promise, `${action} owner ${owner}`);
    }, async kill() { await worker.terminate(); trace.push({ type: 'abrupt-owner-loss', owner }); for (const gate of pending.values()) gate.reject(Error('owner terminated')); pending.clear(); } };
    owners.push(api);
    worker.on('error', error => { ready.reject(error); for (const gate of pending.values()) gate.reject(error); });
    worker.on('message', message => {
      trace.push({ owner, ...message });
      if (message.type === 'ready') ready.resolve(message);
      if (message.type === 'startup-failed') ready.reject(Error(message.error.message));
      if (message.type === 'effect') effects.push({ owner, ...message.effect });
      if (message.type === 'reply') { const gate = pending.get(message.id); pending.delete(message.id); if (message.error) gate.reject(Object.assign(Error(message.error.message), message.error)); else gate.resolve(message.result); }
      wake();
    });
    await within(ready.promise, `fresh owner ${owner} startup`);
    return api;
  }
  async function cleanup() {
    if (cleaned) return; cleaned = true;
    for (const owner of owners) await owner.worker.terminate();
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/durable-children-wasm/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL(label + '.json', output), JSON.stringify({ command: 'node --test js/nanocodex/test/durable-children-wasm.test.mjs', label, trace, effects }, null, 2));
    await rm(directory, { recursive: true, force: true });
  }
  t.after(cleanup);
  return { start, until, trace, effects, requests, phase(value) { phase = value; }, release() { for (const reply of held.splice(0)) reply(); }, cleanup };
}

async function completed(owner, id, stage) {
  const report = await owner.call('wait', { agentIds: [id], timeoutMs: 10_000 });
  assert.equal(report.timed_out, false, JSON.stringify(report));
  assert.equal(report.agents[0].status.state, 'completed', JSON.stringify(report));
  assert.deepEqual(report.agents[0].status.output, { stage });
  return report;
}

test('public WASM cold restart preserves child IDs, results, mailbox, cancellation and committed effects', { timeout: 90_000 }, async t => {
  const f = await fixture(t, 'cold-tree');
  let owner = await f.start();
  const done = await owner.call('spawn', task('COMPLETED'));
  await completed(owner, done.agent_id, 'completed');
  const running = await owner.call('spawn', task('RUNNING'));
  const interrupted = await owner.call('spawn', task('INTERRUPTED'));
  const closed = await owner.call('spawn', task('CLOSED'));
  await f.until(() => ['RUNNING', 'INTERRUPTED', 'CLOSED'].every(marker => f.requests.some(row => row.marker === marker && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT'))), 'all held children checkpoint their effect');
  const mailbox = await owner.call('send', { agentId: running.agent_id, message: 'MAILBOX_AFTER_RESTART', purpose: 'coordinate', priority: 'deferred' });
  assert.equal(mailbox.disposition, 'queued');
  const interruptedReport = await owner.call('interrupt', { agentId: interrupted.agent_id });
  assert.equal(interruptedReport.agents[0].status.state, 'interrupted');
  const closedReport = await owner.call('close', { agentId: closed.agent_id });
  assert.equal(closedReport.agents[0].status.state, 'closed');
  const before = await owner.call('list', { includeCompleted: true });
  const identities = f.effects.map(effect => ({ agentId: effect.subagent.agentId, sessionId: effect.sessionId }));
  assert.equal(new Set(identities.map(row => row.sessionId)).size, 4);
  await owner.kill();
  f.phase('resume');
  owner = await f.start();
  await owner.call('recover');
  await owner.call('recover');
  const recovered = await owner.call('list', { includeCompleted: true });
  assert.deepEqual(recovered.agents.map(row => row.agent_id).sort(), before.agents.map(row => row.agent_id).sort());
  assert.deepEqual(recovered.agents.find(row => row.agent_id === done.agent_id).status, { state: 'completed', output: { stage: 'completed' } });
  assert.equal(recovered.agents.find(row => row.agent_id === closed.agent_id).status.state, 'closed');
  assert.equal(recovered.agents.find(row => row.agent_id === interrupted.agent_id).status.state, 'interrupted');
  await f.until(() => f.trace.some(row => row.type === 'event' && row.owner === 2 && JSON.stringify(row.event).includes('mailbox')), 'recovered mailbox result');
  await completed(owner, running.agent_id, 'mailbox');
  assert.equal(f.effects.length, 4, 'each committed external child effect dispatches once across owner loss');
  assert.ok(f.requests.filter(row => row.phase === 'resume' && row.marker === 'RUNNING').every(row => row.auth === 'Bearer synthetic-owner-2'), 'resumed child uses current host authentication');
  assert.equal(f.requests.filter(row => row.phase === 'resume' && row.marker === 'CLOSED').length, 0, 'closed child never resumes inference');
  await owner.kill();
  owner = await f.start();
  const durableMailbox = await owner.call('list', { includeCompleted: true });
  assert.deepEqual(durableMailbox.agents.find(row => row.agent_id === running.agent_id).status, { state: 'completed', output: { stage: 'mailbox' } });
  assert.equal(f.effects.length, 4);
  t.diagnostic(JSON.stringify({ coldOwners: 3, stableIds: identities, mailbox, effectDispatches: f.effects.length, interrupted: interrupted.agent_id, closed: closed.agent_id }));
});

test('public WASM replay of lost spawn admission acknowledgement returns the same child without duplicate effects', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'spawn-lost-ack');
  let owner = await f.start({ loseSpawnAcknowledgement: true });
  const root = owner.call('prompt', { id: 'root-spawn-once', input: 'ROOT_SPAWN: delegate to DEDUP_CHILD then finish.' });
  await f.until(() => f.trace.some(row => row.type === 'lost-spawn-ack'), 'persisted spawn admission with lost acknowledgement');
  const lostRoot = assert.rejects(root, /owner terminated/);
  await owner.kill();
  await lostRoot; f.phase('resume');
  owner = await f.start();
  const result = await owner.call('prompt', { id: 'root-spawn-once', input: 'ROOT_SPAWN: delegate to DEDUP_CHILD then finish.' });
  assert.equal(result.finalMessage, 'ROOT_COMPLETE');
  const directory = await owner.call('list', { includeCompleted: true });
  assert.equal(directory.agents.length, 1, JSON.stringify(directory));
  await completed(owner, directory.agents[0].agent_id, 'dedup_child');
  assert.equal(f.effects.length, 1);
  t.diagnostic(JSON.stringify({ replayedOperation: 'root-spawn-once', stableChild: directory.agents[0].agent_id, childCount: 1, effectDispatches: 1 }));
});

test('public WASM requires durability for background children and releases foreground only', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'lifetimes');
  const ephemeral = await f.start({ nonDurable: true });
  await assert.rejects(ephemeral.call('spawn', task('BACKGROUND', 'background')), /durab|background/i);
  assert.equal(f.requests.length, 0, 'invalid lifetime fails before inference');
  await ephemeral.kill();
  let owner = await f.start();
  const foreground = await owner.call('spawn', task('FOREGROUND'));
  const background = await owner.call('spawn', task('BACKGROUND', 'background'));
  await f.until(() => ['FOREGROUND', 'BACKGROUND'].every(marker => f.requests.some(row => row.marker === marker && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT'))), 'both lifetimes checkpoint effects');
  await owner.call('shutdown');
  await owner.kill(); f.phase('resume');
  owner = await f.start();
  await owner.call('recover');
  const directory = await owner.call('list', { includeCompleted: true });
  assert.equal(directory.agents.find(row => row.agent_id === foreground.agent_id).status.state, 'closed');
  assert.equal(directory.agents.find(row => row.agent_id === background.agent_id).lifetime, 'background');
  await completed(owner, background.agent_id, 'background');
  assert.equal(f.effects.length, 2);
  assert.equal(f.requests.filter(row => row.phase === 'resume' && row.marker === 'FOREGROUND').length, 0);
  assert.ok(f.requests.filter(row => row.phase === 'resume' && row.marker === 'BACKGROUND').every(row => row.auth === 'Bearer synthetic-owner-3'));
  t.diagnostic(JSON.stringify({ foregroundClosed: foreground.agent_id, backgroundRecovered: background.agent_id, effectDispatches: 2, currentAuthOwner: 3 }));
});


test('public WASM immediate disposal preserves foreground and background checkpoints for a new owner', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'dispose-recovery');
  const oldOwner = await f.start();
  const foreground = await oldOwner.call('spawn', task('FOREGROUND'));
  const background = await oldOwner.call('spawn', task('BACKGROUND', 'background'));
  await f.until(() => ['FOREGROUND', 'BACKGROUND'].every(marker => f.requests.some(row =>
    row.marker === marker && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT'))), 'both lifetimes checkpoint effects');
  await oldOwner.call('dispose');
  // Keep the old JS/WASM isolate alive: disposal must retire its local ownership
  // without depending on process termination or cancelling the durable work.
  f.phase('resume');
  const owner = await f.start();
  await owner.call('recover');
  await completed(owner, foreground.agent_id, 'foreground');
  await completed(owner, background.agent_id, 'background');
  const directory = await owner.call('list', { includeCompleted: true });
  assert.equal(directory.agents.find(row => row.agent_id === background.agent_id).lifetime, 'background');
  assert.equal(f.effects.length, 2, 'committed effects are not repeated after immediate disposal');
  assert.ok(f.requests.filter(row => row.phase === 'resume').every(row => row.auth === 'Bearer synthetic-owner-2'));
  t.diagnostic(JSON.stringify({ foregroundRecovered: foreground.agent_id, backgroundRecovered: background.agent_id, effectDispatches: 2, oldIsolateStillAlive: true }));
});

test('public WASM cold recovery retains the consumed urgent steering revision', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'steer-revision');
  let owner = await f.start();
  const child = await owner.call('spawn', task('RUNNING'));
  await f.until(() => f.requests.some(row => row.marker === 'RUNNING' && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT')), 'child reaches held model boundary');
  const receipt = await owner.call('send', { agentId: child.agent_id, message: 'MAILBOX_AFTER_RESTART', purpose: 'coordinate', priority: 'urgent' });
  assert.equal(receipt.disposition, 'steered');
  f.release();
  await f.until(() => f.requests.some(row => JSON.stringify(row.body).includes('MAILBOX_AFTER_RESTART')), 'native driver consumes the new revision');
  await owner.kill(); f.phase('resume');
  owner = await f.start();
  await completed(owner, child.agent_id, 'mailbox');
  assert.equal(f.effects.length, 1, 'steered recovery preserves the original committed effect');
  const resumed = f.requests.filter(row => row.phase === 'resume');
  assert.ok(resumed.some(row => row.body.input.some(item => item.type === 'function_call_output' && typeof item.output === 'string' && item.output.startsWith('{') && JSON.parse(item.output).accepted === true)), 'revision 2 completion is accepted after cold restore');
  assert.ok(resumed.every(row => row.body.input.filter(item => item.type === 'message' && JSON.stringify(item.content).includes('MAILBOX_AFTER_RESTART')).length === 1), 'the accepted mailbox input is retained once');
  t.diagnostic(JSON.stringify({ child: child.agent_id, disposition: receipt.disposition, effectDispatches: f.effects.length, recoveredOutput: 'mailbox' }));
});

test('public WASM reconciles native urgent admission when the tree delivery acknowledgement is lost', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'steer-lost-delivery');
  let owner = await f.start({ loseSteerDelivery: true });
  const child = await owner.call('spawn', task('RUNNING'));
  await f.until(() => f.requests.some(row => row.marker === 'RUNNING' && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT')), 'child reaches held model boundary');
  const sending = owner.call('send', { agentId: child.agent_id, message: 'MAILBOX_AFTER_RESTART', purpose: 'coordinate', priority: 'urgent' });
  await f.until(() => f.trace.some(row => row.type === 'lost-steer-delivery'), 'native admission committed but tree delivery did not');
  const lost = assert.rejects(sending, /owner terminated/);
  await owner.kill(); await lost; f.phase('resume');
  owner = await f.start();
  await completed(owner, child.agent_id, 'mailbox');
  assert.equal(f.effects.length, 1);
  const resumed = f.requests.filter(row => row.phase === 'resume');
  assert.ok(resumed.length > 0);
  assert.ok(resumed.every(row => row.body.input.filter(item => item.type === 'message' && JSON.stringify(item.content).includes('MAILBOX_AFTER_RESTART')).length <= 1), 'cross-journal replay must not accept a second steer or turn');
  await owner.kill(); owner = await f.start();
  await completed(owner, child.agent_id, 'mailbox');
  assert.equal(f.effects.length, 1);
  t.diagnostic(JSON.stringify({ child: child.agent_id, coldOwners: 3, nativeSteerReceipts: 1, effectDispatches: 1 }));
});

test('public WASM refuses terminal observations after an uncommitted child settlement and recovers the native result', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'failed-settlement');
  let owner = await f.start({ failSettlement: true });
  const child = await owner.call('spawn', task('COMPLETED'));
  await f.until(() => f.trace.some(row => row.type === 'failed-child-settlement'), 'native terminal result encounters failed tree settlement');
  await assert.rejects(owner.call('list', { includeCompleted: true }), /cold recovery|uncommitted|fenced/i);
  await assert.rejects(owner.call('wait', { agentIds: [child.agent_id], timeoutMs: 100 }), /cold recovery|uncommitted|fenced/i);
  await assert.rejects(owner.call('recover'), /cold recovery|uncommitted|fenced/i);
  await owner.kill(); f.phase('resume');
  owner = await f.start();
  await completed(owner, child.agent_id, 'completed');
  assert.equal(f.effects.length, 1, 'recover native completion without redispatching the committed effect');
  assert.equal(f.requests.filter(row => row.phase === 'resume').length, 0, 'native terminal receipt is handed off without more inference');
  t.diagnostic(JSON.stringify({ child: child.agent_id, refusedObservations: ['list', 'wait', 'recover'], effectDispatches: 1, resumedModelRequests: 0 }));
});


test('public WASM reports the child recovery store error and preserves lost-steer work for the next owner', { timeout: 60_000 }, async t => {
  const f = await fixture(t, 'steer-recovery-capacity');
  const first = await f.start({ loseSteerDelivery: true });
  const child = await first.call('spawn', task('RUNNING'));
  await f.until(() => f.requests.some(row => row.marker === 'RUNNING' && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT')), 'child checkpoints its external effect');
  const sending = first.call('send', { agentId: child.agent_id, message: 'MAILBOX_AFTER_RESTART', purpose: 'coordinate', priority: 'urgent' });
  await f.until(() => f.trace.some(row => row.type === 'lost-steer-delivery'), 'native admission committed before losing the tree acknowledgement');
  const lost = assert.rejects(sending, /owner terminated/);
  await first.kill(); await lost; f.phase('resume');
  const sessionId = f.effects[0].sessionId;
  await assert.rejects(f.start({ exhaustChildStorage: sessionId }), /database or disk is full/);
  assert.equal(f.trace.find(row => row.type === 'child-acquisition-error').error.code, 'ERR_SQLITE_ERROR');
  const successor = await f.start();
  await completed(successor, child.agent_id, 'mailbox');
  const resumed = f.requests.filter(row => row.phase === 'resume');
  assert.ok(resumed.length > 0);
  assert.ok(resumed.every(row => row.auth === 'Bearer synthetic-owner-3'));
  assert.ok(resumed.every(row => row.body.input.filter(item => item.type === 'message' && JSON.stringify(item.content).includes('MAILBOX_AFTER_RESTART')).length <= 1), 'failed construction must not admit the mailbox twice');
  assert.equal(f.effects.length, 1, 'the committed effect survives failed recovery without redispatch');
  t.diagnostic(JSON.stringify({ child: child.agent_id, failedOwner: 2, successfulOwner: 3, effectDispatches: 1, underlyingError: 'database or disk is full' }));
});


test('public WASM repeated failed construction retires local generations in the same isolate', { timeout: 90_000 }, async t => {
  const f = await fixture(t, 'failed-create-same-isolate');
  const first = await f.start();
  const foreground = await first.call('spawn', task('FOREGROUND'));
  const background = await first.call('spawn', task('BACKGROUND', 'background'));
  await f.until(() => ['FOREGROUND', 'BACKGROUND'].every(marker => f.requests.some(row =>
    row.marker === marker && JSON.stringify(row.body).includes('DURABLE_CHILD_EFFECT_RECEIPT'))), 'both children retain their committed effects');
  await first.kill();
  // Keep one worker/module alive across every rejected create and recovery.
  const owner = await f.start({ manualCreate: true });
  const childSession = f.effects.find(row => row.subagent.agentId === String(foreground.agent_id)).sessionId;
  const fail = async index => {
    const failed = assert.rejects(owner.call('create', { exhaustChildStorage: childSession, auth: `failed-constructor-${index}` }), /database or disk is full/);
    // Recovery visits the second child first. Hold the first child's store
    // acquisition until the recovered sibling has a real request in flight.
    await f.until(() => f.requests.some(row => row.auth === `Bearer failed-constructor-${index}`), 'partial recovery starts its sibling');
    await owner.call('releaseAcquire');
    await failed;
  };
  for (let index = 0; index < 8; index++) await fail(index);
  const warm = (await owner.call('memory')).bytes;
  for (let index = 8; index < 48; index++) await fail(index);
  const retained = (await owner.call('memory')).bytes;
  assert.equal(retained, warm, 'rejected constructors must release their WASM capabilities rather than grow linearly');
  const oldRequests = f.requests.filter(row => row.auth?.startsWith('Bearer failed-constructor-'));
  assert.equal(oldRequests.length, 48, 'each failure follows partial sibling recovery');
  await f.until(() => oldRequests.every(row => row.closed), 'failed generations abort all local model requests');
  f.phase('resume');
  await owner.call('create', { auth: 'current-constructor' });
  await completed(owner, foreground.agent_id, 'foreground');
  await completed(owner, background.agent_id, 'background');
  const directory = await owner.call('list', { includeCompleted: true });
  assert.deepEqual(directory.agents.map(row => row.agent_id).sort(), [foreground.agent_id, background.agent_id].sort());
  assert.equal(f.effects.length, 2, 'committed effects are not dispatched again');
  assert.ok(f.requests.filter(row => row.phase === 'resume').every(row => row.auth === 'Bearer current-constructor'));
  assert.ok(!directory.agents.some(row => row.status.state === 'closed'), 'construction failure must not cancel durable foreground records');
  t.diagnostic(JSON.stringify({ failedCreatesInOneIsolate: 48, warmBytes: warm, retainedBytes: retained,
    recoveredIds: directory.agents.map(row => row.agent_id), effectDispatches: f.effects.length, oldRequests: oldRequests.length }));
});
