import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { Agent, Transport } from '../host/index.mjs';
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from '../runtime/durability-store.mjs';

// Only inference is a fixture. Public SDK, WASM, owner fencing, immutable
// indexes, transactions and cold SQLite reopen all use their shipped paths.
test('public WASM session documents commit atomically and fork historical data after receipt pruning', { timeout: 60_000 }, async (t) => {
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const directory = await mkdtemp(join(tmpdir(), 'nanocodex-documents-'));
  const requests = [];
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event('open'))); }
    close() { this.readyState = 3; }
    send(encoded) {
      const request = JSON.parse(encoded); requests.push(request);
      queueMicrotask(() => this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify({
        type: 'response.completed', response: { id: `response-${requests.length}`, status: 'completed', end_turn: true,
          output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: `answer-${requests.length}` }] }],
          usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } },
      }) })));
    }
  }
  const transport = () => Transport.openAi({ apiKey: 'synthetic-destination-key', WebSocketImpl: ModelSocket, websocketWarmup: false });
  let db;
  let agent;
  const open = () => {
    db = new DatabaseSync(join(directory, 'session.sqlite'));
    sqliteDurabilitySchema.forEach(sql => db.exec(sql));
    return createSqliteDurabilityStore({ transaction(fn) {
      db.exec('BEGIN IMMEDIATE');
      try {
        const result = fn((sql, args = []) => db.prepare(sql).all(...args));
        db.exec('COMMIT'); return result;
      } catch (error) { db.exec('ROLLBACK'); throw error; }
    } });
  };
  const create = (durability, durabilityId, extra = {}) => Agent.create({ module, tools: [], transport: transport(), durability, durabilityId, terminalReceiptRetention: 1, ...extra });
  const write = (key, expectedVersion, value, fork = 'asOf') => ({ key, expectedVersion, value, fork });
  try {
    let store = open();
    agent = await create(store, 'source');
    assert.equal(await agent.session.document('absent'), null);
    await agent.session.compareExchangeDocuments(['initial', 'current', 'asOf'].map(fork => write(fork, 0, 1, fork)));
    await agent.turn.prompt({ id: 'historical-0', input: 'FIRST_BOUNDARY' }).result();
    await agent.session.compareExchangeDocuments(['initial', 'current', 'asOf'].map(fork => write(fork, 1, 2, fork)));
    const before = await store.load('source');
    await assert.rejects(agent.session.compareExchangeDocuments([write('asOf', 2, 99), write('current', 1, 99, 'current')]), /version/i);
    assert.deepEqual(await store.load('source'), before, 'rejected multiwrite publishes neither partial values nor revision');
    await assert.rejects(agent.session.compareExchangeDocuments([write('asOf', 2, 99, 'initial')]), /policy/i);
    for (let index = 1; index < 5; index++) await agent.turn.prompt({ id: `historical-${index}`, input: `LATER_BOUNDARY_${index}` }).result();
    await agent.session.compareExchangeDocuments([write('late', 0, 'omitted')]);
    await agent.session.shutdown(); agent = undefined; db.close();
    store = open(); agent = await create(store, 'source');
    const seed = await agent.session.documentFork('historical-0');
    assert.equal(seed.documents.boundary, 'historical-0');
    assert.deepEqual(Object.keys(seed.documents.documents).sort(), ['asOf', 'current', 'initial']);
    assert.deepEqual(['initial', 'current', 'asOf'].map(key => seed.documents.documents[key].value), [1, 2, 1]);
    assert.equal(seed.documents.documents.asOf.fork, 'asOf');
    const callsBefore = requests.length;
    await assert.rejects(agent.turn.prompt({ id: 'historical-0', input: 'FIRST_BOUNDARY' }).result(), /terminal/i);
    assert.equal(requests.length, callsBefore, 'pruned operation ID cannot rerun inference');
    await agent.session.compareExchangeDocuments([write('block', 0, 'private', 'block')]);
    await agent.turn.prompt({ id: 'blocked-boundary', input: 'BLOCKED_BOUNDARY' }).result();
    await assert.rejects(agent.session.documentFork('blocked-boundary'), /block/i);
    await agent.session.shutdown(); agent = undefined;
    agent = await create(store, 'child', { documentFork: seed });
    assert.equal((await agent.session.document('asOf')).value, 1);
    await agent.session.compareExchangeDocuments([write('asOf', seed.documents.documents.asOf.version, 42)]);
    await agent.session.shutdown(); agent = undefined; db.close();
    store = open(); agent = await create(store, 'child');
    assert.equal((await agent.session.document('asOf')).value, 42);
    await agent.turn.prompt({ id: 'child-next', input: 'CHILD_ONLY' }).result();
    const childRequest = JSON.stringify(requests.at(-1));
    assert.match(childRequest, /FIRST_BOUNDARY/);
    assert.doesNotMatch(childRequest, /LATER_BOUNDARY|BLOCKED_BOUNDARY/);
    await assert.rejects(create(store, 'child', { documentFork: seed }), /pristine|empty|existing|already|occupied/i);
    await agent.session.shutdown(); agent = await create(store, 'source');
    assert.equal((await agent.session.document('asOf')).value, 2, 'branch mutation never changes the parent');
    t.diagnostic(JSON.stringify({ completedParentTurns: 6, receiptRetention: 1, historicalBoundary: seed.documents.boundary,
      selectedValues: [1, 2, 1], coldChildValue: 42, parentValue: 2, requests: requests.length }));
  } finally { await agent?.session.shutdown().catch(() => {}); db?.close(); await rm(directory, { recursive: true, force: true }); }
});
