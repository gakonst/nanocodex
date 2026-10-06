// Real SDK/WASM and SQLite; only the remote Messages provider is synthetic.
// node --test js/nanocodex/test/cloudflare-claude-reconstruction-wasm.test.mjs
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { create } from '../cloudflare/Agent.mjs';
import { Claude } from '../host/index.mjs';

function sqliteStorage(database) {
  let transaction = 0;
  return {
    sql: { exec(sql, ...args) {
      const rows = database.prepare(sql).all(...args);
      return { toArray: () => rows, [Symbol.iterator]: () => rows[Symbol.iterator]() };
    } },
    transactionSync(callback) {
      const name = `transaction_${transaction++}`;
      database.exec(`SAVEPOINT ${name}`);
      try { const value = callback(); database.exec(`RELEASE ${name}`); return value; }
      catch (error) { database.exec(`ROLLBACK TO ${name}; RELEASE ${name}`); throw error; }
    },
  };
}

function owner(storage, id) {
  return {
    ctx: { id: { toString: () => id }, storage, acceptWebSocket() {}, getWebSockets() { return []; } },
    env: { NANOCODEX: { fetch() { throw new Error('Claude must use Messages transport'); } } },
  };
}

function message(text) {
  return [
    { type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'claude-opus-5-5', content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(frame => `data: ${JSON.stringify(frame)}\n\n`).join('');
}

for (const seeded of [false, true]) test(`Cloudflare Claude reconstructs ${seeded ? 'a retained conversation' : 'before its first prompt'} without losing replacement ownership`, { timeout: 30_000 }, async t => {
  const trace = [], results = [], errors = [];
  const database = new DatabaseSync(':memory:');
  const storage = sqliteStorage(database);
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      trace.push(body);
      assert.equal(body.model, 'claude-opus-5-5');
      const text = seeded && trace.length === 1 ? 'SEED_CONTEXT_RETAINED' : 'CONTINUATION_COMPLETE';
      if (seeded && trace.length > 1) assert.match(JSON.stringify(body.messages), /SEED_CONTEXT_RETAINED/);
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(message(text));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const agents = [];
  t.after(async () => {
    for (const agent of agents.reverse()) await agent.session.shutdown().catch(() => {});
    database.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const options = {
    eventPersistence: 'caller',
    [Symbol.for('nanocodex.cloudflare.internalConfiguration')]: {
      model: 'claude-opus-5-5', thinking: 'low', reasoning_mode: 'standard', fast_mode: false,
    },
    [Symbol.for('nanocodex.cloudflare.internalRuntime')]: {
      claude: { create: options => Claude.create({ ...options,
        auth: { apiKey: 'synthetic' }, endpoint: `http://127.0.0.1:${server.address().port}/v1/messages`,
      }) },
    },
  };
  const sameOwner = 'a'.repeat(64), otherOwner = 'b'.repeat(64);
  const construct = async id => {
    const agent = await create(module, owner(storage, id), options);
    agents.push(agent);
    return agent;
  };
  const fence = () => database.prepare('SELECT owner_id, fence FROM nanocodex_durable_owners').get();
  let failure;
  try {
    const first = await construct(sameOwner);
    if (seeded) {
      results.push(await first.turn.prompt({ input: 'Remember the seed context.', id: 'seed' }).result());
      assert.equal(results[0].finalMessage, 'SEED_CONTEXT_RETAINED');
    }
    const before = fence();
    const replacement = await construct(sameOwner);
    assert.equal(replacement.sessionId, first.sessionId);
    const after = fence();
    assert.notEqual(after.owner_id, before.owner_id);
    assert.ok(BigInt(after.fence) > BigInt(before.fence));
    first.dispose();
    await assert.rejects(construct(otherOwner), /already active/);
    assert.deepEqual(fence(), after, 'different owner is rejected before altering the fence');
    results.push(await replacement.turn.prompt({ input: 'Continue using the remembered context.', id: 'continuation' }).result());
    assert.equal(results.at(-1).finalMessage, 'CONTINUATION_COMPLETE');
    assert.deepEqual(errors, []);
    assert.equal(trace.length, seeded ? 2 : 1);
  } catch (error) { failure = error; throw error; }
  finally {
    const directory = new URL('../../../output/cloudflare-claude-reconstruction/', import.meta.url);
    await mkdir(directory, { recursive: true });
    await writeFile(new URL(seeded ? 'retained-conversation.json' : 'first-prompt.json', directory), JSON.stringify({ status: failure ? 'failed' : 'passed', error: failure?.stack, results, trace, errors }, null, 2));
  }
});
