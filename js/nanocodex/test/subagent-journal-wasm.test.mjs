// Real SDK/WASM, Code Mode and SQLite; only the remote Messages provider is synthetic.
// node --test js/nanocodex/test/subagent-journal-wasm.test.mjs
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { create } from '../cloudflare/Agent.mjs';
import { Claude, Subagents } from '../host/index.mjs';
import { initializeBrowserEngine } from '../browser/engine.mjs';
import { codeEvaluator } from './quickjs-fixture.mjs';

const CHILDREN = 4;
const STEPS = 12;
const OUTPUT_BYTES = 24 * 1024;

function sqliteStorage(database, journalWrites) {
  let transaction = 0;
  return {
    sql: { exec(sql, ...args) {
      if (/INSERT INTO nanocodex_durable_states/.test(sql) && String(args[0]).endsWith(':subagents')) {
        journalWrites.push(Buffer.byteLength(String(args[2])));
      }
      const rows = database.prepare(sql).all(...args);
      return { toArray: () => rows, one: () => rows[0], [Symbol.iterator]: () => rows[Symbol.iterator]() };
    } },
    transactionSync(callback) {
      const name = 't' + transaction++;
      database.exec('SAVEPOINT ' + name);
      try { const value = callback(); database.exec('RELEASE ' + name); return value; }
      catch (error) { database.exec('ROLLBACK TO ' + name + '; RELEASE ' + name); throw error; }
    },
  };
}

const owner = (storage, id) => ({
  ctx: { id: { toString: () => id }, storage, acceptWebSocket() {}, getWebSockets() { return []; } },
  env: { NANOCODEX: { fetch() { throw new Error('Claude must use Messages transport'); } } },
});

let messageId = 0;
function frames(blocks) {
  const out = [{ type: 'message_start', message: { id: 'msg_' + (++messageId), role: 'assistant', model: 'claude-opus-5-5', content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((block, index) => {
    if (block.type === 'tool_use') {
      out.push({ type: 'content_block_start', index, content_block: { type: 'tool_use', id: block.id, name: block.name, input: {} } });
      out.push({ type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } });
    } else {
      out.push({ type: 'content_block_start', index, content_block: { type: 'text', text: block.text } });
    }
    out.push({ type: 'content_block_stop', index });
  });
  out.push({ type: 'message_delta', delta: { stop_reason: blocks.some(b => b.type === 'tool_use') ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 1 } });
  out.push({ type: 'message_stop' });
  return out.map(frame => 'data: ' + JSON.stringify(frame) + '\n\n').join('');
}

// Each child grows its conversation through real Code Mode tool results, then
// submits its result. The root only answers its own warmup prompt.
function reply(body) {
  const text = JSON.stringify(body.messages);
  const child = /CHILD-(\d+)/.exec(text)?.[1];
  const results = (text.match(/"type":"tool_result"/g) ?? []).length;
  if (child === undefined) return [{ type: 'text', text: 'root ready' }];
  if (/SUBMITTED-OK/.test(text) || results > STEPS) return [{ type: 'text', text: 'done' }];
  if (results === STEPS) return [{ type: 'tool_use', id: 'toolu_s' + child, name: 'exec',
    input: { code: 'await tools.submit_result({output:{child:' + child + '}}); text("SUBMITTED-OK")' } }];
  return [{ type: 'tool_use', id: 'toolu_' + child + '_' + results, name: 'exec',
    input: { code: 'text("step ' + results + ' " + "y".repeat(' + OUTPUT_BYTES + '))' } }];
}

async function settled(agent, count) {
  const deadline = Date.now() + 60_000;
  for (;;) {
    const agents = (await Subagents.list(agent, { includeCompleted: true })).agents;
    if (agents.length === count && agents.every(a => !['pending', 'running'].includes(a.status.state))) return agents;
    assert.ok(Date.now() < deadline, 'children must settle');
    await new Promise(resolve => setTimeout(resolve, 25));
  }
}

test('subagent journal writes stay small as child conversations grow, and restore every child', { timeout: 120_000 }, async t => {
  const errors = [];
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(frames(reply(JSON.parse(Buffer.concat(chunks)))));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const journalWrites = [];
  const database = new DatabaseSync(':memory:');
  const storage = sqliteStorage(database, journalWrites);
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const engine = await initializeBrowserEngine({ module });
  const endpoint = 'http://127.0.0.1:' + server.address().port + '/v1/messages';
  const options = {
    codeEvaluator,
    eventPersistence: 'caller',
    [Symbol.for('nanocodex.cloudflare.internalConfiguration')]: {
      model: 'claude-opus-5-5', thinking: 'low', reasoning_mode: 'standard', fast_mode: false,
    },
    [Symbol.for('nanocodex.cloudflare.internalRuntime')]: {
      subagentsEnabled: true,
      claude: { create: claude => Claude.create({ ...claude, subagents: { maxConcurrency: CHILDREN },
        auth: { apiKey: 'synthetic' }, endpoint, compatibilityProfile: 'subscription',
        subscriptionIdentity: { installId: 'journal-test', platform: 'linux', arch: 'x64' } }) },
    },
  };
  const id = 'd'.repeat(64);
  let agent = await create(module, owner(storage, id), options);
  t.after(async () => {
    await agent.session.shutdown().catch(() => {});
    database.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  await agent.turn.prompt({ input: 'root warmup' }).result();
  for (let child = 0; child < CHILDREN; child++) {
    await Subagents.spawn(agent, { role: 'worker' + child, task: 'CHILD-' + child + ' grow the conversation', outputSchema: { type: 'object' } });
  }
  const live = await settled(agent, CHILDREN);
  assert.deepEqual(live.map(child => child.status.state), Array(CHILDREN).fill('completed'));
  assert.deepEqual(errors, []);

  // Each child retained roughly STEPS * OUTPUT_BYTES of conversation. The
  // journal references those checkpoints instead of rewriting all of them.
  const largest = Math.max(...journalWrites);
  t.diagnostic(JSON.stringify({ journalWrites: journalWrites.length, largestJournalBytes: largest,
    totalJournalBytes: journalWrites.reduce((a, b) => a + b, 0), wasmBytes: engine.memory.buffer.byteLength }));
  assert.ok(journalWrites.length > CHILDREN * STEPS, 'every child step is journaled');
  assert.ok(largest < 64 * 1024, 'journal writes must not embed child conversations: ' + largest);

  // A reconstructed runtime restores every child, with its checkpoint, from the records.
  await agent.session.shutdown();
  agent = await create(module, owner(storage, id), options);
  const restored = (await Subagents.list(agent, { includeCompleted: true })).agents;
  assert.deepEqual(restored.map(child => child.role).sort(), live.map(child => child.role).sort());
  assert.deepEqual(restored.map(child => child.status.state), Array(CHILDREN).fill('completed'));
  assert.ok(restored.every(child => child.can_message), 'restored children keep their conversations');

  // Journals written before checkpoint records embedded each conversation
  // (version 1). They still restore, and the next write references records.
  await agent.session.shutdown();
  const journalRow = database.prepare("SELECT state_id, payload FROM nanocodex_durable_states WHERE state_id LIKE '%:subagents'").get();
  const stored = key => database.prepare('SELECT value FROM nanocodex_durable_records WHERE state_id = ? AND key = ?').get(journalRow.state_id, key).value;
  const readRecord = key => {
    const value = stored(key);
    if (value.startsWith('=')) return value.slice(1);
    assert.ok(value.startsWith('#'), 'checkpoint records are inline or chunk manifests');
    return value.slice(1).match(/.{64}/g).map(hash => stored('c:' + hash)).join('');
  };
  const current = JSON.parse(journalRow.payload);
  assert.equal(current.version, 2);
  const legacy = { version: 1, agents: current.agents.map(({ checkpoint_ref, ...rest }) => ({ ...rest, ...JSON.parse(readRecord(checkpoint_ref)) })) };
  assert.ok(legacy.agents.every(child => child.native_checkpoint?.payload), 'Claude children carry native checkpoints');
  database.prepare('UPDATE nanocodex_durable_states SET payload = ? WHERE state_id = ?').run(JSON.stringify(legacy), journalRow.state_id);
  journalWrites.length = 0;
  agent = await create(module, owner(storage, id), options);
  const upgraded = (await Subagents.list(agent, { includeCompleted: true })).agents;
  assert.deepEqual(upgraded.map(child => child.status.state), Array(CHILDREN).fill('completed'));
  const deadline = Date.now() + 10_000;
  while (JSON.parse(database.prepare('SELECT payload FROM nanocodex_durable_states WHERE state_id = ?').get(journalRow.state_id).payload).version !== 2) {
    assert.ok(Date.now() < deadline, 'a restored legacy journal is rewritten with checkpoint references');
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  assert.ok(Math.max(...journalWrites) < 64 * 1024);
});
