import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { Worker } from 'node:worker_threads';
import test from 'node:test';
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from '../runtime/durability-store.mjs';
import { startResponsesServer, messageReader, sendCompleted, sendFinal, sendWarmup } from './support/responses.mjs';

// This adapter is the external persistent boundary, not a substitute runtime.
// It stores the real SDK's durability requests and journal admissions/receipts.
function persistentBoundary(database, parallelReads, lostAcknowledgement, lostAdmissionAcknowledgement) {
  let acknowledgementLost = false;
  const readAdmissions = new Set();
  let releaseReads = [];
  for (const statement of sqliteDurabilitySchema) database.exec(statement);
  database.exec(`CREATE TABLE effects (key TEXT PRIMARY KEY, hash TEXT NOT NULL, generation INTEGER NOT NULL, receipt TEXT);
    CREATE TABLE dispatches (kind TEXT NOT NULL, call_id TEXT NOT NULL);`);
  const query = (sql, args = []) => database.prepare(sql).all(...args);
  const durability = createSqliteDurabilityStore({ transaction(callback) {
    database.exec('BEGIN IMMEDIATE');
    try { const value = callback(query); database.exec('COMMIT'); return value; }
    catch (error) { database.exec('ROLLBACK'); throw error; }
  } });
  let generation = 0;
  return {
    nextGeneration: () => ++generation,
    run(owner, method, args) {
      if (owner !== generation) throw new Error('old journal owner fenced');
      if (method.startsWith('durability.')) return durability[method.slice(11)](...args);
      if (method === 'effect') {
        database.prepare('INSERT INTO dispatches VALUES (?, ?)').run(...args);
        if (['poison', 'abortable'].includes(args[0])) return new Promise(() => {});
        if (parallelReads && ['read-one', 'read-two'].includes(args[0])) {
          readAdmissions.add(args[0]);
          if (readAdmissions.size === 2) { for (const resolve of releaseReads) resolve(null); releaseReads = []; }
          else return new Promise(resolve => releaseReads.push(resolve));
        }
        return null;
      }
      const [context, receipt] = args;
      const key = JSON.stringify([context.sessionId, context.operationId ?? "", context.modelCallIndex ?? 0, context.parentCallId, context.callId]);
      const hash = createHash('sha256').update(JSON.stringify([context.source, context.name, context.input])).digest('hex');
      const row = database.prepare('SELECT * FROM effects WHERE key = ?').get(key);
      if (row && row.hash !== hash) throw new Error('original source/input fence conflict');
      if (method === 'journal.begin') {
        if (row) return row.receipt === null ? { status: 'unknown' } : { status: 'replay', receipt: JSON.parse(row.receipt) };
        database.prepare('INSERT INTO effects VALUES (?, ?, ?, NULL)').run(key, hash, generation);
        if (lostAdmissionAcknowledgement) throw new Error('lost durable admission acknowledgement');
        return { status: 'execute' };
      }
      assert.equal(method, 'journal.complete');
      assert.equal(row?.generation, generation, 'only original intent owner may complete');
      assert.equal(row.receipt, null);
      database.prepare('UPDATE effects SET receipt = ? WHERE key = ?').run(JSON.stringify(receipt), key);
      if (lostAcknowledgement && !acknowledgementLost) {
        acknowledgementLost = true; throw new Error('lost durable completion acknowledgement');
      }
      return null;
    },
  };
}

async function journey(t, { sdk = 'node', journal = true, source, expected, oversized = false, evaluator = "quickjs", label = 'pending', cancellation = false, discarded = false, parallelReads = false, lostAcknowledgement = false, media = false, lostAdmissionAcknowledgement = false, reusedProviderIds = false, queuedProviderIds = false }) {
  const directory = await mkdtemp(join(tmpdir(), 'nanocodex-code-recovery-'));
  const database = new DatabaseSync(join(directory, 'recovery.sqlite'));
  database.exec('PRAGMA busy_timeout = 5000');
  const boundary = persistentBoundary(database, parallelReads, lostAcknowledgement, lostAdmissionAcknowledgement);
  const server = await startResponsesServer();
  const trace = [];
  const workers = [];
  let worker;
  const messages = [];
  const waiters = [];
  const push = value => { messages.push(value); for (const waiter of [...waiters]) waiter(); };
  function until(predicate) {
    return new Promise(resolve => {
      const check = () => { const value = messages.find(predicate); if (value) { waiters.splice(waiters.indexOf(check), 1); resolve(value); } };
      waiters.push(check); check();
    });
  }
  function start() {
    const owner = boundary.nextGeneration();
    worker = new Worker(new URL('./support/code-recovery-owned.worker.mjs', import.meta.url), {
      workerData: { sdk, journal, reusedProviderIds, queuedProviderIds: queuedProviderIds && owner > 1, url: server.url, cancellation, evaluator, databasePath: join(directory, 'recovery.sqlite') },
    });
    const started = worker;
    workers.push(worker);
    worker.on('error', error => { console.error('Recovery worker error', error); push({ type: 'worker-error', error }); });
    worker.on('message', async message => {
      trace.push({ owner, ...message });
      if (process.env.NANOCODEX_RECOVERY_DEBUG) console.error('worker', owner, message.type, message.method ?? message.error ?? '');
      if (message.type !== 'rpc') { push({ owner, ...message }); return; }
      if (message.method === 'effect') push({ type: 'dispatch', owner, kind: message.args[0] });
      try {
        const result = await boundary.run(owner, message.method, message.args);
        started.postMessage({ id: message.id, result });
      } catch (error) { started.postMessage({ id: message.id, error: error.message }); }
    });
    return owner;
  }
  let cleaned = false;
  t.after(cleanup);
  try {
    let owner = start();
    let socket = await server.nextConnection();
    let reader = messageReader(socket);
    let request = await reader.next();
    if (request.generate === false) { sendWarmup(socket, 'warmup-fixture'); request = await reader.next(); }
    trace.push({ owner, type: 'model-request', request });
    if (process.env.NANOCODEX_RECOVERY_DEBUG) console.error('model', request.type, request.generate, request.input?.length);
    sendCompleted(socket, 'synthetic-code', [{ type: 'custom_tool_call', call_id: 'owned-cell', name: 'exec', input: source }]);
    if (discarded) {
      // The cell has ended, but an unawaited, abort-ignoring handler is still pending.
      const endedRequest = await reader.next();
      trace.push({ owner, type: 'discarded-cell-model-request', request: endedRequest });
      const unknowns = trace.filter(entry => entry.type === 'event' && entry.event.type === 'tool.result'
        && entry.event.payload.call_id === 'owned-cell/code-1');
      assert.equal(unknowns.length, 1, 'discarded invocation has exactly one terminal telemetry receipt');
      assert.equal(unknowns[0].event.payload.status, 'failed');
      assert.equal(unknowns[0].event.payload.structured_result.outcome, 'unknown');
    } else if (oversized) {
      const settledRequest = await reader.next();
      trace.push({ owner, type: 'terminal-unknown-model-request', request: settledRequest });
      const output = settledRequest.input.find(item => item.call_id === 'owned-cell'
        && item.type === 'custom_tool_call_output');
      assert.ok(output, 'deterministically invalid receipt settles through the real SDK transport');
      assert.match(JSON.stringify(output.output), /outcome unknown/);
      assert.doesNotMatch(JSON.stringify(output.output), /journal interrupted|CAUGHT_AND_RETRIED/);
      assert.equal(messages.filter(message => message.type === 'failure').length, 0);
    } else await until(message => message.owner === owner && message.type === (lostAcknowledgement || lostAdmissionAcknowledgement ? 'failure' : 'dispatch')
      && (lostAcknowledgement || lostAdmissionAcknowledgement || message.kind === (cancellation ? 'abortable' : 'poison')));
    if (cancellation) {
      worker.postMessage({ action: 'cancel' });
      const failure = await until(message => message.owner === owner && message.type === 'failure');
      assert.match(failure.error.message, /cancel/i);
      const nextSocket = await server.nextConnection();
      const nextReader = messageReader(nextSocket);
      trace.push({ owner, type: 'after-cancel-model-request', request: await nextReader.next() });
      sendFinal(nextSocket, 'synthetic-after-cancel', 'FOLLOW_ON_OK');
      assert.equal((await until(message => message.owner === owner && message.type === 'follow-on')).finalMessage, 'FOLLOW_ON_OK');
      const effects = database.prepare('SELECT receipt FROM effects').all();
      assert.equal(effects.length, 1);
      assert.equal(effects[0].receipt, null, 'aborted dispatched effect must retain unknown, not replayable cancellation error');
      assert.deepEqual(database.prepare('SELECT kind FROM dispatches').all().map(row => row.kind), ['abortable']);
      t.diagnostic('Public turn.cancel() unblocked follow-on; cancelled dispatched intent remains unknown, guest retry never dispatched.');
      return;
    }
    if (lostAcknowledgement || lostAdmissionAcknowledgement) assert.match(messages.find(m => m.owner === owner && m.type === 'failure').error.message, /journal interrupted/);
    // No shutdown, disposal, cancellation or final tool receipt. This is owner loss.
    await worker.terminate();
    trace.push({ owner, type: 'abrupt-terminate' });
    socket.terminate();
    owner = start();
    if (!journal) {
      await until(message => message.owner === owner && message.type === 'dispatch' && message.kind === 'poison');
      assert.deepEqual(database.prepare('SELECT kind, COUNT(*) AS count FROM dispatches GROUP BY kind ORDER BY kind').all()
        .map(row => ({ ...row })), [{ kind: 'poison', count: 2 }, { kind: 'read-one', count: 2 }, { kind: 'read-two', count: 2 }]);
      t.diagnostic('Negative control: real abrupt restart without journal redispatched both completed reads and poison twice.');
      return;
    }
    socket = await server.nextConnection();
    reader = messageReader(socket);
    const recoveredRequest = await reader.next();
    trace.push({ owner, type: 'recovered-model-request', request: recoveredRequest });
    const cellOutput = recoveredRequest.input.find(item => item.type === "custom_tool_call_output" && item.call_id === "owned-cell");
    assert.ok(cellOutput, "real Rust transport received the recovered cell receipt");
    const encoded = typeof cellOutput.output === "string" ? cellOutput.output
      : cellOutput.output.map(item => item.text ?? "").join("");
    if (!discarded && !lostAcknowledgement && !media) assert.match(encoded, /outcome unknown/, 'recovered pending intent becomes an explicit failed cell');
    assert.doesNotMatch(encoded, /CAUGHT_AND_RETRIED/);
    if (lostAcknowledgement || discarded) assert.match(encoded, /Script completed/);
    for (const output of expected ?? []) assert.ok(encoded.includes(output), `replayed output contains ${output}`);
    if (media) {
      assert.ok(Array.isArray(cellOutput.output));
      const image = cellOutput.output.find(item => item.type === "input_image");
      assert.ok(image, "recovered inline image reaches the real model transport");
      const bytes = Buffer.from(image.image_url.split(",")[1], "base64");
      assert.ok(bytes.length > 2 * 1024 * 1024);
      assert.equal(bytes.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
      // The public tool.result preserves the requested 100,000-token cell budget.
      // Provider history has an independent existing 12,000-token context cap.
      const cellReceipt = trace.find(entry => entry.owner === owner && entry.type === "event"
        && entry.event.type === "tool.result" && entry.event.payload.call_id === "owned-cell");
      assert.ok(cellReceipt);
      const text = cellReceipt.event.payload.result.find(item => item.type === "input_text" && item.text.startsWith("MAX_OUTPUT_BEGIN"));
      assert.ok(trace.some(entry => entry.owner === owner && entry.type === "event"
        && entry.event.type === "tool.result" && entry.event.payload.structured_result?.outcome === "unknown"));
      assert.ok(text && text.text.endsWith("MAX_OUTPUT_END"));
      assert.equal(text.text.length, 399000, "100,000-token output budget preserves near-400KB output");
      const receipts = database.prepare("SELECT receipt FROM effects WHERE receipt IS NOT NULL").all().map(row => JSON.parse(row.receipt));
      const receipt = receipts.find(item => item.structured_result?.content?.[0]?.type === "image");
      assert.equal(receipt.valueRef, "structured_result");
      assert.equal(receipt.outputJsonRef, "structured_result");
      assert.equal(receipt.output, null);
      assert.equal(receipt.value, null);
      assert.equal(image.image_url, "data:image/png;base64," + receipt.structured_result.content[0].data);
      assert.ok(Buffer.byteLength(JSON.stringify(receipt)) < 8 * 1024 * 1024);
      t.diagnostic(JSON.stringify({ mediaBytes: bytes.length, receiptBytes: Buffer.byteLength(JSON.stringify(receipt)), textBytes: text.text.length }));
    }
    if (reusedProviderIds) {
      await reusedCall('read-one', 'synthetic-reused-same-model');
      await reusedCall('read-two', 'synthetic-reused-different-model');
    }
    sendFinal(socket, 'synthetic-recovered', 'RECOVERED');
    assert.equal((await until(message => message.owner === owner && message.type === 'result')).finalMessage, 'RECOVERED');
    const followRequest = await reader.next();
    trace.push({ owner, type: 'follow-on-model-request', request: followRequest });
    async function reusedCall(kind, responseId) {
      sendCompleted(socket, responseId, [{ type: 'custom_tool_call', call_id: 'owned-cell', name: 'exec', input: `text(await tools.effect({kind:'${kind}'}));` }]);
      const request = await reader.next();
      trace.push({ owner, type: 'reused-provider-call-output', kind, request });
      const output = request.input.filter(item => item.type === 'custom_tool_call_output'
        && item.call_id === 'owned-cell').at(-1);
      assert.ok(output);
      const text = typeof output.output === 'string' ? output.output : output.output.map(item => item.text ?? '').join('');
      assert.match(text, new RegExp(kind)); assert.doesNotMatch(text, /outcome unknown|journal interrupted/);
    }
    if (reusedProviderIds) await reusedCall('read-one', 'synthetic-reused-same');
    sendFinal(socket, 'synthetic-follow', 'FOLLOW_ON_OK');
    assert.equal((await until(message => message.owner === owner && message.type === 'follow-on')).finalMessage, 'FOLLOW_ON_OK');
    if (reusedProviderIds) {
      trace.push({ owner, type: 'third-turn-model-request', request: await reader.next() });
      await reusedCall('read-two', 'synthetic-reused-different');
      sendFinal(socket, 'synthetic-third', 'THIRD_TURN_OK');
      assert.equal((await until(message => message.owner === owner && message.type === 'third-on')).finalMessage, 'THIRD_TURN_OK');
      const contexts = trace.filter(entry => entry.type === 'rpc' && entry.method === 'journal.begin').map(entry => entry.args[0]);
      assert.equal(contexts[0].operationId, contexts[1].operationId, 'original operation identity survives abrupt owner loss');
      assert.equal(contexts[0].modelCallIndex, contexts[1].modelCallIndex);
      assert.deepEqual([...new Set(contexts.map(context => context.operationId))], ['original', 'follow-on', 'third-turn']);
      assert.ok(contexts.every(context => context.modelCallIndex >= 1));
      const aliasOperations = new Map();
      for (const entry of trace.filter(entry => entry.type === 'rpc' && entry.method === 'journal.begin')) {
        const context = entry.args[0];
        const alias = JSON.stringify([entry.owner, context.turnId]);
        if (aliasOperations.has(alias)) assert.equal(aliasOperations.get(alias), context.operationId,
          'one live owner never reuses a WASM ABI lookup alias for a different original operation');
        aliasOperations.set(alias, context.operationId);
      }
      assert.equal(new Set(contexts.map(context => context.parentCallId)).size, 1, 'provider parent ID is actually reused');
      assert.equal(new Set(contexts.map(context => context.callId)).size, 1, 'provider call ID is actually reused');
      if (queuedProviderIds) {
        assert.ok(trace.some(entry => entry.type === 'queued-operation-submitted'));
        const accepted = trace.findIndex(entry => entry.owner === owner && entry.type === 'event'
          && entry.event.type === 'input.accepted' && entry.event.payload.request_id === 'follow-on');
        const originalLater = trace.findIndex(entry => entry.owner === owner && entry.type === 'rpc'
          && entry.method === 'journal.begin' && entry.args[0].operationId === 'original' && entry.args[0].modelCallIndex === 3);
        assert.ok(accepted >= 0 && accepted < originalLater, 'later queued input is accepted before original operation dispatches its next call');
      }
      assert.deepEqual([...new Set(contexts.filter(context => context.operationId === 'original').map(context => context.modelCallIndex))], [1, 2, 3]);
    }
    const dispatches = database.prepare('SELECT kind, COUNT(*) AS count FROM dispatches GROUP BY kind ORDER BY kind').all().map(row => ({ ...row }));
    if (reusedProviderIds) assert.deepEqual(dispatches, [{ kind: 'read-one', count: 3 }, { kind: 'read-two', count: 2 }]);
    else assert.ok(dispatches.every(row => row.count === 1), JSON.stringify(dispatches));
    assert.ok(!dispatches.some(row => row.kind === 'retry'));
    if (lostAdmissionAcknowledgement) assert.equal(dispatches.length, 0, 'no dispatch before intent ACK');
    const effects = database.prepare('SELECT key, generation, receipt IS NOT NULL AS completed FROM effects ORDER BY key').all().map(row => ({ ...row }));
    assert.equal(effects.filter(row => row.completed === 0).length, lostAcknowledgement ? 0 : 1,
      'only genuinely uncertain original intents remain pending for reconciliation');
    t.diagnostic(JSON.stringify({ sdk, dispatches, effects, original: 'RECOVERED', followOn: 'FOLLOW_ON_OK' }));
  } finally { await cleanup(); }
  async function cleanup() {
    if (cleaned) return;
    cleaned = true;
    for (const item of workers) await item.terminate();
    await server.close();
    // Optional inspectable per-run trace; never commit generated evidence.
    if (process.env.NANOCODEX_RECOVERY_TRACE_DIR) {
      await writeFile(join(process.env.NANOCODEX_RECOVERY_TRACE_DIR, `code-recovery-${sdk}-${label}-${journal}.json`), JSON.stringify(trace, null, 2));
    }
    database.close();
    await rm(directory, { recursive: true, force: true });
  }
}

const source = `const values = await Promise.all([tools.effect({kind:'read-one'}), tools.effect({kind:'read-two'})]);
text(values);
try { await tools.effect({kind:'poison'}); } catch (error) {
  await tools.effect({kind:'retry'}); text('CAUGHT_AND_RETRIED');
}`;
for (const sdk of ['node', 'host', 'cloudflare']) test(`${sdk} owned SDK: abrupt Rust WASM/QuickJS restart replays reads, fences pending write, unblocks original and follow-on`,
  { timeout: 30000 }, t => journey(t, { sdk, source, parallelReads: true }));
test('negative control: abrupt Rust WASM/QuickJS restart actually repeats nested effects without opt-in journal',
  { timeout: 30000 }, t => journey(t, { journal: false, source, parallelReads: true }));
test('owned SDK: failed/undefined Promise.allSettled receipts survive abrupt restart without new dispatch',
  { timeout: 30000 }, t => journey(t, { label: 'errors', source: `
const settled = await Promise.allSettled(['undefined','raw-undefined','raw-error','typed-error','failed-result'].map(kind => tools.effect({kind})));
text(settled.map(r => r.status === 'fulfilled' ? {status:r.status,undefined:r.value === undefined}
 : {status:r.status,undefined:r.reason === undefined,code:r.reason && r.reason.code,name:r.reason && r.reason.name,details:r.reason && r.reason.details}));
await tools.effect({kind:'poison'});`, expected: [JSON.stringify([{status:'fulfilled',undefined:true},{status:'rejected',undefined:true},
  {status:'rejected',undefined:false,code:'RAW'}, {status:'rejected',undefined:false,code:'TYPED',name:'Error',details:{retry:false}},
  {status:'rejected',undefined:false,code:'FAILED_RESULT',name:'Error'}])] }));
test('owned SDK: oversized post-effect receipt leaves original intent unknown and never redispatches',
  { timeout: 30000 }, t => journey(t, { label: 'oversized', oversized: true, source: `try { text(await tools.effect({kind:'huge'})); } catch (error) { await tools.effect({kind:'retry'}); }` }));

test('owned SDK: cancelled dispatched tool stays unknown while follow-on remains usable',
  { timeout: 30000 }, t => journey(t, { label: 'cancelled', cancellation: true,
    source: `try { await tools.effect({kind:'abortable'}); } catch (error) { await tools.effect({kind:'retry'}); }` }));
test('owned SDK: discarded pending promise closes once and stays fenced across abrupt restart',
  { timeout: 30000 }, t => journey(t, { label: 'discarded', discarded: true,
    source: `void tools.effect({kind:'poison'}); text(await tools.effect({kind:'read-one'}));` }));

test('owned node SDK: default evaluator preserves error code/details across abrupt WASM restart',
  { timeout: 30000 }, t => journey(t, { label: 'default-errors', evaluator: 'default', source: `
try { await tools.effect({kind:'typed-error'}); } catch (error) {
  text({code:error.code,details:error.details});
}
await tools.effect({kind:'poison'});`, expected: ['TYPED', '"retry":false'] }));

test('owned SDK: lost durable completion acknowledgement replays receipt without a second write',
  { timeout: 30000 }, t => journey(t, { label: 'lost-ack', lostAcknowledgement: true,
    source: `try { text(await tools.effect({kind:'read-one'})); } catch (error) { await tools.effect({kind:'retry'}); }`,
    expected: ['read-one', '"done":true'] }));

test('owned SDK: ~2 MiB PNG and 100,000-token text output survive journalled abrupt restart',
  { timeout: 60000 }, t => journey(t, { sdk: 'cloudflare', label: 'media-output', media: true,
    source: `// @exec: {"max_output_tokens":100000}
const result = await tools.effect({kind:'media'});
image(result.content[0]);
text(await tools.effect({kind:'max-output'}));
await tools.effect({kind:'poison'});` }));

test('owned Code Mode SDK: lost intent ACK interrupts before dispatch, restart reports unknown and next turn healthy',
  { timeout: 30000 }, t => journey(t, { label: 'intent-ack', lostAdmissionAcknowledgement: true,
    source: "await tools.effect({kind:'poison'});" }));

test('owned Code Mode SDK: reused provider call IDs across model responses and operations dispatch fresh same and different inputs',
  { timeout: 30000 }, t => journey(t, { reusedProviderIds: true, lostAcknowledgement: true,
    label: 'nested-reused-turns',
    source: "text(await tools.effect({kind:'read-one'}));", expected: ['read-one'] }));

test('owned Code Mode SDK: queued operation while original effect outstanding preserves canonical identity',
  { timeout: 30000 }, t => journey(t, { queuedProviderIds: true, reusedProviderIds: true, lostAcknowledgement: true,
    label: 'nested-queued-identity',
    source: "text(await tools.effect({kind:'read-one'}));", expected: ['read-one'] }));

for (const sdk of ['node', 'host']) test('owned ' + sdk + ': journal supports real non-durable child-style prompts with explicit null operation IDs', { timeout: 20_000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'nanocodex-ephemeral-journal-'));
  const database = new DatabaseSync(join(directory, 'recovery.sqlite'));
  const boundary = persistentBoundary(database, false, false, false);
  const owner = boundary.nextGeneration();
  const server = await startResponsesServer();
  const trace = [];
  const worker = new Worker(new URL('./support/code-recovery-owned.worker.mjs', import.meta.url), {
    workerData: { sdk, journal: true, nonDurable: true, url: server.url },
  });
  let resolveFollow, rejectFollow;
  const followed = new Promise((resolve, reject) => { resolveFollow = resolve; rejectFollow = reject; });
  worker.on('error', rejectFollow);
  worker.on('message', async message => {
    trace.push(message);
    if (message.type === 'failure') rejectFollow(Error(message.error.message));
    if (message.type === 'follow-on') resolveFollow(message);
    if (message.type !== 'rpc') return;
    try { worker.postMessage({ id: message.id, result: await boundary.run(owner, message.method, message.args) }); }
    catch (error) { worker.postMessage({ id: message.id, error: error.message }); }
  });
  t.after(async () => { await worker.terminate(); await server.close(); database.close(); await rm(directory, {recursive:true,force:true}); });
  const socket = await server.nextConnection();
  const reader = messageReader(socket);
  for (const [index, kind] of ['read-one', 'read-two'].entries()) {
    let request = await reader.next();
    if (request.generate === false) { sendWarmup(socket, 'warmup'); request = await reader.next(); }
    sendCompleted(socket, 'ephemeral-tool-' + index, [{type:'custom_tool_call',call_id:'reused-cell',name:'exec',input:'text(await tools.effect({kind:' + JSON.stringify(kind) + '}));'}]);
    await reader.next();
    sendFinal(socket, 'ephemeral-final-' + index, index === 0 ? 'FIRST_OK' : 'FOLLOW_ON_OK');
  }
  assert.equal((await followed).finalMessage, 'FOLLOW_ON_OK');
  const inputs = trace.filter(x => x.type === 'event' && x.event.type === 'input.accepted');
  assert.equal(inputs.length, 2);
  assert.ok(inputs.every(x => x.event.payload.request_id === null));
  const effects = database.prepare('SELECT key,receipt FROM effects').all();
  assert.equal(effects.length, 2);
  assert.ok(effects.every(x => JSON.parse(x.key)[1].startsWith('non-durable:') && x.receipt !== null));
  assert.notEqual(JSON.parse(effects[0].key)[1], JSON.parse(effects[1].key)[1]);
  assert.deepEqual(database.prepare('SELECT kind FROM dispatches').all().map(x => x.kind), ['read-one','read-two']);
});
