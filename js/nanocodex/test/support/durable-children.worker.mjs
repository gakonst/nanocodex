// A fresh public SDK/WASM isolate. SQLite is the only state carried across owners.
import { parentPort, workerData } from 'node:worker_threads';
import { readFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { Agent, Subagents, Transport } from '../../host/index.mjs';
import { initializeBrowserEngine } from '../../browser/engine.mjs';
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from '../../runtime/durability-store.mjs';

const database = new DatabaseSync(workerData.databasePath);
database.exec('PRAGMA busy_timeout = 5000');
for (const sql of sqliteDurabilitySchema) database.exec(sql);
const store = createSqliteDurabilityStore({ transaction(callback) {
  try {
    database.exec('BEGIN IMMEDIATE');
    const result = callback((sql, args) => database.prepare(sql).all(...args));
    database.exec('COMMIT');
    return result;
  } catch (error) {
    parentPort.postMessage({ type: 'store-error', error: { message: error.message, code: error.code, stack: error.stack } });
    if (database.isTransaction) database.exec('ROLLBACK');
    throw error;
  }
} });
let acquisitionFailure;
let exhaustChildStorage = workerData.exhaustChildStorage;
let lostAcknowledgement = false, nativeSteerCommitted = false, settlementFailed = false;
const durability = { ...store, async acquire(id, request) {
  if (id === exhaustChildStorage) {
    if (workerData.manualCreate) await new Promise(resolve => { acquisitionFailure = resolve; });
    // A real SQLite capacity error at child recovery, without filling the disk
    // or changing the committed journals. SQLITE_FULL rolls back this probe.
    const limit = database.prepare('PRAGMA max_page_count').get().max_page_count;
    const pages = database.prepare('PRAGMA page_count').get().page_count;
    database.exec(`PRAGMA max_page_count = ${pages}`);
    try {
      database.exec('BEGIN IMMEDIATE');
      database.exec('CREATE TABLE synthetic_capacity_probe (value BLOB)');
      database.exec('INSERT INTO synthetic_capacity_probe VALUES (zeroblob(1048576))');
      throw new Error('SQLite capacity probe unexpectedly succeeded');
    } catch (error) {
      if (database.isTransaction) database.exec('ROLLBACK');
      parentPort.postMessage({ type: 'child-acquisition-error', stateId: id, error: { message: error.message, code: error.code } });
      throw error;
    } finally {
      database.exec(`PRAGMA max_page_count = ${limit}`);
    }
  }
  return store.acquire(id, request);
}, replace(id, request) {
  const encoded = request.payload + request.records.map(record => record.value).join('');
  if (workerData.failSettlement && !settlementFailed && id.endsWith('/children') && encoded.includes('"state":"completed"')) {
    settlementFailed = true;
    parentPort.postMessage({ type: 'failed-child-settlement', stateId: id });
    throw new Error('synthetic definitely uncommitted child settlement');
  }
  if (workerData.loseSteerDelivery && nativeSteerCommitted && id.endsWith('/children')) {
    parentPort.postMessage({ type: 'lost-steer-delivery', stateId: id });
    return new Promise(() => {});
  }
  const result = store.replace(id, request);
  if (workerData.loseSteerDelivery && !id.endsWith('/children') && encoded.includes('"child-message:1"')) {
    nativeSteerCommitted = true;
    parentPort.postMessage({ type: 'native-steer-committed', stateId: id });
  }
  // The host committed the admission but its acknowledgement was lost.
  // All assertions about the resulting child use the public SDK below.
  if (workerData.loseSpawnAcknowledgement && !lostAcknowledgement && id.endsWith('/children')) {
    // Observe the committed immutable records at the external store boundary;
    // the current child journal head is an opaque versioned envelope.
    if (request.records.some(record => record.value.includes('"Spawn"'))) {
      lostAcknowledgement = true;
      parentPort.postMessage({ type: 'lost-spawn-ack', stateId: id });
      // Keep the committed write's response in flight until this process dies.
      // A returned error would be a received acknowledgement which the model
      // can handle as an ordinary failed tool call.
      return new Promise(() => {});
    }
  }
  return result;
} };
let agent;
const module = await readFile(new URL('../../pkg-web/nanocodex_bg.wasm', import.meta.url));
async function create(options = {}) {
  exhaustChildStorage = options.exhaustChildStorage ?? workerData.exhaustChildStorage;
  agent = await Agent.create({
    module,
    model: 'gpt-6.1-sol', thinking: 'low', toolMode: 'direct',
    sessionId: '018f1f9a-7b3c-7a07-8000-000000000077',
    ...(workerData.nonDurable ? {} : { durability, durabilityId: 'durable-children-public' }),
    transport: Transport.openAi({ apiKey: options.auth ?? workerData.auth, apiBaseUrl: workerData.baseUrl, stateless: true }),
    tools: { boundary: { description: 'A pure synthetic model boundary.', parameters: { type: 'object', properties: {}, additionalProperties: false }, handler() { return 'BOUNDARY_RECEIPT'; } }, proof: {
      description: 'Append a synthetic external effect with its public child identity.',
      parameters: { type: 'object', properties: {}, additionalProperties: false },
      handler(_input, context) {
        const effect = { sessionId: context.sessionId, subagent: context.subagent, callId: context.callId, parentCallId: context.parentCallId };
        parentPort.postMessage({ type: 'effect', effect });
        return 'DURABLE_CHILD_EFFECT_RECEIPT';
      },
    } },
  });
  agent.events.watch({ includeAllSessions: true }).onEvent(event => parentPort.postMessage({ type: 'event', event }));
  return { sessionId: agent.sessionId };
}
try {
  const result = workerData.manualCreate ? {} : await create();
  parentPort.postMessage({ type: 'ready', ...result });
} catch (error) {
  parentPort.postMessage({ type: 'startup-failed', error: { message: error.message, code: error.code } });
}
parentPort.on('message', async ({ id, action, args }) => {
  try {
    let result;
    if (action === 'create') result = await create(args);
    else if (action === 'releaseAcquire') { acquisitionFailure(); acquisitionFailure = undefined; result = {}; }
    else if (action === 'memory') result = { bytes: (await initializeBrowserEngine({ module })).memory.buffer.byteLength };
    else if (action === 'prompt') result = { finalMessage: (await agent.turn.prompt(args).result()).finalMessage };
    else if (action === 'dispose') { agent.dispose(); result = { disposed: true }; }
    else if (action === 'shutdown') { await agent.session.shutdown(); result = { stopped: true }; }
    else if (['interrupt', 'close'].includes(action)) result = await Subagents[action](agent, args.agentId);
    else result = await Subagents[action](agent, args);
    parentPort.postMessage({ type: 'reply', id, result });
  } catch (error) { parentPort.postMessage({ type: 'reply', id, error: { message: error.message, code: error.code } }); }
});
