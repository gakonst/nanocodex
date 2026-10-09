// Public Claude SDK, actual Rust WASM and QuickJS; only Messages HTTP is synthetic.
// Rebuild first: pnpm --filter nanocodex-vite build:wasm
// Run: node --test js/nanocodex/test/claude-code-event-bounds-wasm.test.mjs
// Large nested tool results stay complete for guest code and the model, while
// the archived/broadcast tool.result events carry a bounded preview.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Claude, createQuickJsEvaluator } from '../host/index.mjs';
import asyncVariant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

function messages(block, stop) {
  return [
    { type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'fixture', content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: block },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(frame => 'data: ' + JSON.stringify(frame) + '\n\n').join('');
}

test('Claude Code Mode bounds nested tool.result events without changing guest values', { timeout: 60_000 }, async t => {
  const trace = [], errors = [], events = [];
  let step = 0;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); trace.push(body);
      const history = JSON.stringify(body.messages);
      const block = step++ === 0
        ? { type: 'tool_use', id: 'page-cell', name: 'exec', input: { code:
          'let total = 0, last;\nfor (let i = 0; i < 4; i++) { const page = await tools.BigPage({ page: i }); total += JSON.stringify(page).length; last = page.events.at(-1).marker; }\n'
          + 'text({ total, last, small: await tools.Small({}) });' } }
        : (assert.match(history, /PAGE_3_LAST/), assert.match(history, /SMALL_OK/), { type: 'text', text: 'BOUNDED_OK' });
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(messages(block, block.type === 'text' ? 'end_turn' : 'tool_use'));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  // ~600 KB structured page, comparable to an admin_threads read page.
  const page = index => ({ events: Array.from({ length: 300 }, (_, i) => ({ cursor: String(i),
    marker: i === 299 ? 'PAGE_' + index + '_LAST' : 'event', body: 'x'.repeat(2000) })) });
  let agent, failure, observed;
  try {
    agent = await Claude.create({
      endpoint: 'http://127.0.0.1:' + server.address().port + '/v1/messages',
      model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic' },
      module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)),
      codeEvaluator: createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant)),
      tools: [
        { name: 'BigPage', description: 'Return a large synthetic page', handler: input => page(input.page) },
        { name: 'Small', description: 'Return a small synthetic value', handler: () => ({ status: 'SMALL_OK' }) },
      ],
    });
    const stop = agent.events.watch().onEvent(event => { if (event.type === 'tool.result') events.push(event.payload); });
    t.after(() => stop?.());
    assert.equal((await agent.turn.prompt({ input: 'Page through a large synthetic thread' }).result()).finalMessage, 'BOUNDED_OK');
    assert.deepEqual(errors, []);
    // The guest observed every complete page; the model received its output.
    const modelResult = JSON.stringify(trace[1].messages.at(-1));
    assert.match(modelResult, new RegExp('\\\\"total\\\\":' + JSON.stringify(page(0)).length * 4));
    const sizes = events.map(payload => ({ tool: payload.tool, bytes: JSON.stringify(payload).length,
      truncated: payload.truncated ?? false, original: payload.original_bytes ?? null }));
    observed = sizes;
    const big = events.filter(payload => payload.tool === 'BigPage');
    assert.equal(big.length, 4);
    for (const payload of big) {
      assert.equal(payload.truncated, true);
      assert.ok(payload.original_bytes > 600_000, String(payload.original_bytes));
      assert.equal(payload.structured_result, null);
      assert.ok(JSON.stringify(payload).length < 40_000, String(JSON.stringify(payload).length));
      assert.equal(payload.parent_call_id, 'page-cell');
    }
    const small = events.find(payload => payload.tool === 'Small');
    assert.deepEqual(small.structured_result, { status: 'SMALL_OK' });
    assert.equal(small.truncated, undefined);
    const cell = events.find(payload => payload.call_id === 'page-cell');
    assert.equal(cell.metadata._nanocodex_code.nested_call_count, 5);
    assert.ok(JSON.stringify(cell).length < 40_000, String(JSON.stringify(cell).length));
    assert.ok(sizes.reduce((sum, entry) => sum + entry.bytes, 0) < 200_000);
  } catch (error) { failure = error; throw error; }
  finally {
    await agent?.session.shutdown().catch(() => {});
    const output = new URL('../../../output/claude-code-event-bounds/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('bounded-events.json', output), JSON.stringify({ status: failure ? 'failed' : 'passed',
      error: failure?.stack, events: observed, errors }, null, 2));
  }
});
