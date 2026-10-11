// Real SDK/WASM and SQLite; only the remote Messages provider is synthetic.
// The Cloudflare host's live-input ingress (route) on a Claude Agent created
// through the Cloudflare adapter's agent.extend wrappers: idle input starts a
// turn, active input steers it, and a started turn keeps Claude's host-route
// ownership through cancellation so the next live input starts cleanly.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { create, route } from '../cloudflare/Agent.mjs';
import { Claude } from '../host/index.mjs';
import { codeEvaluator } from './quickjs-fixture.mjs';

function sqliteStorage(database) {
  let transaction = 0;
  return {
    sql: { exec(sql, ...args) {
      const rows = database.prepare(sql).all(...args);
      return { toArray: () => rows, [Symbol.iterator]: () => rows[Symbol.iterator]() };
    } },
    transactionSync(callback) {
      const name = 'transaction_' + transaction++;
      database.exec('SAVEPOINT ' + name);
      try { const value = callback(); database.exec('RELEASE ' + name); return value; }
      catch (error) { database.exec('ROLLBACK TO ' + name + '; RELEASE ' + name); throw error; }
    },
  };
}

function sse(blocks, stop = 'end_turn') {
  const frames = [{ type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'claude-opus-5-5', content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((block, index) => frames.push({ type: 'content_block_start', index, content_block: block }, { type: 'content_block_stop', index }));
  frames.push({ type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } }, { type: 'message_stop' });
  return frames.map(frame => 'event: ' + frame.type + '\ndata: ' + JSON.stringify(frame) + '\n\n').join('');
}
const hold = id => sse([{ type: 'tool_use', id, name: 'exec', input: { code: 'text(await tools.hold({}))' } }], 'tool_use');
const reply = text => sse([{ type: 'text', text }]);

test('Cloudflare live input routes a Claude Agent through shared turn ownership, steering, and cancellation', { timeout: 30_000 }, async t => {
  const requests = [];
  const scripted = [hold('first'), reply('ROUTED_DONE'), hold('second'), reply('AFTER_CANCEL'), hold('third'), reply('AFTER_DISPOSE')];
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    requests.push(JSON.parse(Buffer.concat(chunks)));
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    // A cancelled turn may abandon a request; script by observed tool state.
    response.end(scripted.shift() ?? reply('UNEXPECTED'));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const database = new DatabaseSync(':memory:');
  let agent;
  t.after(async () => {
    await agent?.session.shutdown().catch(() => {});
    database.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  let held = Promise.withResolvers();
  let release = Promise.withResolvers();
  const signals = [];
  const holdTool = { name: 'hold', description: 'Held synthetic tool', handler: async (_input, context) => {
    signals.push(context.signal);
    held.resolve();
    await release.promise;
    return 'held';
  } };
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  agent = await create(module, {
    ctx: { id: { toString: () => 'c'.repeat(64) }, storage: sqliteStorage(database), acceptWebSocket() {}, getWebSockets() { return []; } },
    env: { NANOCODEX: { fetch() { throw new Error('Claude must use Messages transport'); } } },
  }, {
    codeEvaluator,
    eventPersistence: 'caller',
    [Symbol.for('nanocodex.cloudflare.internalConfiguration')]: {
      model: 'claude-opus-5-5', thinking: 'low', reasoning_mode: 'standard', fast_mode: false,
    },
    [Symbol.for('nanocodex.cloudflare.internalRuntime')]: {
      claude: { create: options => Claude.create({ ...options, tools: [holdTool],
        auth: { apiKey: 'synthetic' }, endpoint: 'http://127.0.0.1:' + server.address().port + '/v1/messages',
      }) },
    },
  });

  // Idle live input starts a turn; input during the held tool steers that turn.
  const first = await route(agent, { input: 'book the flight' });
  assert.ok(first, 'idle live input starts a turn');
  const firstResult = first.result();
  await held.promise;
  assert.equal(await route(agent, { input: 'and ask for a window seat' }), undefined);
  release.resolve();
  assert.equal((await firstResult).finalMessage, 'ROUTED_DONE');
  assert.match(JSON.stringify(requests[1].messages), /and ask for a window seat/);

  // A routed turn cancels its own held Code Mode cell and settles.
  held = Promise.withResolvers();
  release = Promise.withResolvers();
  const second = await route(agent, { input: 'start another booking' });
  assert.ok(second);
  await held.promise;
  await second.cancel();
  // The routed turn's owner cancels exactly its host-side tool work.
  assert.equal(signals.at(-1).aborted, true, 'cancelling a routed turn aborts its held tool');
  await assert.rejects(second.result(), error => error.code === 'cancelled');
  release.resolve();

  // Its host routes were released with the terminal receipt: new live input
  // starts a fresh turn rather than steering the cancelled one.
  const third = await route(agent, { input: 'try again' });
  assert.ok(third, 'live input after cancellation starts a new turn');
  assert.equal((await third.result()).finalMessage, 'AFTER_CANCEL');
  assert.doesNotMatch(JSON.stringify(requests.at(-1).messages), /UNEXPECTED/);

  // A started routed turn owns the Agent's host routes until its terminal
  // receipt: releasing the Agent handle mid-tool does not strand that turn.
  held = Promise.withResolvers();
  release = Promise.withResolvers();
  const fourth = await route(agent, { input: 'finish after release' });
  const fourthResult = fourth.result();
  await held.promise;
  agent.dispose();
  agent = undefined;
  release.resolve();
  assert.equal((await fourthResult).finalMessage, 'AFTER_DISPOSE');
});
