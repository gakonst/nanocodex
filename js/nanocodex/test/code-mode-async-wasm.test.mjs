import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { test } from 'node:test';
import { Agent, Transport } from '../host/index.mjs';
import { createWorkerEvaluator } from '../runtime/worker-evaluator.mjs';
import { NodeWebWorker } from './support/node-web-worker.mjs';

function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function response(index, output, endTurn = false) {
  return new Response(`data: ${JSON.stringify({ type: 'response.completed', response: {
    id: `async-surface-${index}`, status: 'completed', end_turn: endTurn, output,
    usage: { input_tokens: 100, output_tokens: 5, total_tokens: 105 },
  } })}\n\n`, { headers: { 'content-type': 'text/event-stream' } });
}
function names(tools) { return tools.flatMap(tool => tool.type === 'namespace' ? tool.tools.map(item => item.name) : [tool.name ?? tool.type]); }

for (const enabled of [true, false]) test(`real WASM catalog and native controls: async=${enabled}`, { timeout: 30000 }, async () => {
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const gate = deferred();
  const completed = deferred();
  const tasks = [];
  const trace = [];
  let requests = 0;
  const agent = await Agent.create({
    module, model: 'gpt-6-astra', thinking: 'low', mcp: false, rawApiEvents: false,
    codeEvaluator: createWorkerEvaluator({ createWorker: () => new NodeWebWorker(new URL('../runtime/code-evaluator.worker.mjs', import.meta.url)) }),
    tools: { fixture_gate: { description: 'Synthetic retained gate', parameters: { type: 'object' }, handler: async () => { await gate.promise; return 'released'; } } },
    ...(enabled ? { codeAsyncJobs: {
      enabled: () => true,
      admit: async context => { trace.push({ event: 'admitted', context }); return { jobId: 'native-control-job', status: 'execute' }; },
      complete: async (context, receipt) => { trace.push({ event: 'completed', context, receipt }); completed.resolve(receipt); },
      retain: task => tasks.push(task),
    } } : {}),
    transport: Transport.hostManaged({ stateless: true, websocketPreconnect: false, apiBaseUrl: 'https://provider.invalid/v1',
      createWebSocket() { assert.fail('SSE fixture only'); },
      async createResponse(_endpoint, _session, request) {
        const input = JSON.parse(request.body);
        trace.push({ event: 'model-request', request: input });
        requests++;
        const exposed = names(input.tools ?? input.input.filter(item => item.type === 'additional_tools').at(-1)?.tools ?? []);
        if (enabled) assert.deepEqual(exposed, ['exec']);
        else {
          assert.ok(exposed.includes('exec'), JSON.stringify(exposed));
          assert.ok(exposed.includes('wait'), JSON.stringify(exposed));
          assert.ok(exposed.includes('list_agents'), JSON.stringify(exposed));
        }
        if (requests === 1) return response(1, enabled
          ? [{ type: 'custom_tool_call', name: 'exec', call_id: 'native-control-origin', input: 'await tools.fixture_gate({}); text(await tools.list_agents({ include_self: true }));' }]
          : [{ type: 'function_call', name: 'list_agents', call_id: 'legacy-native', arguments: '{"include_self":true}' }]);
        assert.equal(requests, 2);
        const output = input.input.find(item => item.call_id === (enabled ? 'native-control-origin' : 'legacy-native') && item.type.endsWith('_output'));
        assert.ok(output, JSON.stringify(input.input));
        if (enabled) assert.match(JSON.stringify(output), /native-control-job/);
        else assert.match(JSON.stringify(output), /agents/);
        return response(2, [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: enabled ? 'ADMITTED' : 'LEGACY_OK' }] }], true);
      },
    }),
  });
  try {
    const result = await agent.turn.prompt({ input: 'Inspect the synthetic native agent directory.' }).result();
    assert.equal(result.finalMessage, enabled ? 'ADMITTED' : 'LEGACY_OK');
    if (enabled) {
      gate.resolve();
      const receipt = await completed.promise;
      await Promise.all(tasks);
      assert.equal(receipt.success, true, JSON.stringify(receipt));
      assert.deepEqual(receipt.nested_calls.map(call => call.name), ['fixture_gate', 'list_agents']);
      assert.equal(receipt.nested_calls[1].success, true);
      assert.ok(Array.isArray(receipt.nested_calls[1].structured_result.agents));
    }
    await mkdir('output', { recursive: true });
    await writeFile(`output/async-surface-wasm-${enabled}.json`, JSON.stringify(trace, null, 2));
  } finally { gate.resolve(); await agent.session.shutdown(); }
});
