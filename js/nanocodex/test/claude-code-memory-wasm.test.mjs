// Public Claude SDK, actual Rust WASM and QuickJS; only Messages HTTP is synthetic.
// Rebuild first: pnpm --filter nanocodex-vite build:wasm
// Run: node --test js/nanocodex/test/claude-code-memory-wasm.test.mjs
// Production Claude DO 1496bf75 exceeded its isolate memory limit right after
// one yielded cell made ~105 sequential session_control reads. Nested results
// already delivered to the model through exec/wait must not stay reachable for
// the rest of the turn, while every result is still delivered exactly once.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { setFlagsFromString } from 'node:v8';
import { runInNewContext } from 'node:vm';
import { Claude } from '../host/index.mjs';
import { codeEvaluator } from './quickjs-fixture.mjs';

setFlagsFromString('--expose-gc');
const gc = runInNewContext('gc');
const heap = () => { gc(); gc(); return process.memoryUsage().heapUsed; };
const READS = 105, CELLS = 24, PAGE_BYTES = 512 * 1024;

function messages(block, stop) {
  return [
    { type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'fixture', content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: block },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(frame => 'data: ' + JSON.stringify(frame) + '\n\n').join('');
}
const resultText = body => {
  const block = body.messages.at(-1)?.content?.findLast?.(item => item.type === 'tool_result');
  if (!block) return '';
  return typeof block.content === 'string' ? block.content
    : block.content.filter(item => item.type === 'text').map(item => item.text).join('\n');
};

test('Claude Code Mode releases delivered nested results during a long turn', { timeout: 120_000 }, async t => {
  const errors = [], events = [], execIds = [], cellTotals = [];
  let waits = 0, cells = 0, handled = 0, baseline = 0, measured;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const last = resultText(body);
      let block;
      const running = /Script running with cell ID (\S+)/.exec(last)?.[1];
      if (body.messages.length === 1) {
        execIds.push('cell-yield');
        // A yielded cell that pages a large history, polled by wait like the model.
        block = { type: 'tool_use', id: 'cell-yield', name: 'exec', input: { code: '// @exec: {"yield_time_ms": 50}\n'
          + 'let total = 0; for (let page = 0; page < ' + READS + '; page++) total += (await tools.Events({ page })).events[0].body.length;\n'
          + 'text("PAGES_DONE " + total);' } };
      } else if (running) {
        block = { type: 'tool_use', id: 'wait-' + ++waits, name: 'wait', input: { cell_id: running, yield_time_ms: 50 } };
      } else if (cells < CELLS) {
        if (cells === 0) assert.match(last, new RegExp('PAGES_DONE ' + READS * PAGE_BYTES));
        else cellTotals.push(last.match(/CELL_\d+ \d+/)?.[0]);
        const id = 'cell-' + cells++;
        execIds.push(id);
        // Many short sequential cells in the same turn.
        block = { type: 'tool_use', id, name: 'exec', input: { code:
          'const a = await tools.Events({ page: 0 }); const b = await tools.Events({ page: 1 });\n'
          + 'text("' + id.replace('cell-', 'CELL_') + ' " + (a.events[0].body.length + b.events[0].body.length));' } };
      } else {
        cellTotals.push(last.match(/CELL_\d+ \d+/)?.[0]);
        // Still inside the turn: everything above was delivered to the model.
        const retained = heap() - baseline;
        measured = { retained_heap_bytes: retained, max_rss_kb: process.resourceUsage().maxRSS };
        block = { type: 'text', text: 'MEMORY_OK' };
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(messages(block, block.type === 'text' ? 'end_turn' : 'tool_use'));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  let agent, failure, summary;
  try {
    agent = await Claude.create({
      endpoint: 'http://127.0.0.1:' + server.address().port + '/v1/messages',
      model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic' },
      module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)),
      codeEvaluator,
      tools: [{ name: 'Events', description: 'Return one large synthetic history page',
        inputSchema: { type: 'object', properties: { page: { type: 'number' } } },
        handler: async input => {
          handled++;
          await new Promise(resolve => setTimeout(resolve, 2));
          return { events: [{ cursor: String(input.page), body: String(input.page % 10).repeat(PAGE_BYTES) }] };
        } }],
    });
    const stop = agent.events.watch().onEvent(event => { if (event.type === 'tool.result') events.push(event.payload); });
    t.after(() => stop?.());
    baseline = heap();
    assert.equal((await agent.turn.prompt({ input: 'Page through a long synthetic history' }).result()).finalMessage, 'MEMORY_OK');
    assert.deepEqual(errors, []);
    const nested = events.filter(payload => payload.tool === 'Events');
    const ids = nested.map(payload => payload.call_id);
    summary = { reads: handled, payload_bytes: handled * PAGE_BYTES, waits, cells, nested_results: nested.length,
      unique_nested_call_ids: new Set(ids).size, ...measured };
    console.log(JSON.stringify(summary));
    assert.equal(handled, READS + 2 * CELLS, 'each nested read ran once');
    assert.ok(waits > 0, 'the paging cell yielded and was resumed by wait');
    // Every nested result reached the event stream exactly once, under its exec cell.
    assert.equal(nested.length, handled);
    assert.equal(new Set(ids).size, handled);
    for (const payload of nested) assert.ok(execIds.includes(payload.parent_call_id), payload.parent_call_id);
    assert.deepEqual(cellTotals, Array.from({ length: CELLS }, (_, cell) => 'CELL_' + cell + ' ' + 2 * PAGE_BYTES));
    // ~77 MiB was delivered; only bounded live state may remain reachable.
    assert.ok(measured.retained_heap_bytes < 32 * 1024 * 1024,
      'delivered nested results remain reachable: ' + measured.retained_heap_bytes + ' bytes');
  } catch (error) { failure = error; throw error; }
  finally {
    await agent?.session.shutdown().catch(() => {});
    const output = new URL('../../../output/claude-code-memory/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('summary.json', output), JSON.stringify({ status: failure ? 'failed' : 'passed',
      error: failure?.stack, summary, errors }, null, 2));
  }
});
