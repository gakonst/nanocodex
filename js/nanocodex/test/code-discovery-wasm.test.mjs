import assert from 'node:assert/strict';
import test from 'node:test';
import { Agent, Transport } from '../node/index.mjs';
import { startResponsesServer, messageReader, sendCompleted, sendFinal, sendWarmup } from './support/responses.mjs';

const largeDescription = 'Lookup the complete synthetic catalog. ' + 'é'.repeat(8000);
const deferredSchema = { type: 'object', properties: { key: { type: 'string', description: 'Required synthetic lookup key' } }, required: ['key'], additionalProperties: false };

async function evaluator(kind) {
  if (kind === 'native') return undefined;
  const { default: variant } = await import('@jitl/quickjs-wasmfile-release-asyncify');
  const { newQuickJSAsyncWASMModuleFromVariant } = await import('quickjs-emscripten-core');
  const { createQuickJsEvaluator } = await import('../runtime/quickjs-evaluator.mjs');
  return createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(variant));
}

for (const [kind, budget] of [['native', 0], ['native', 32], ['quickjs', undefined], ['quickjs', 6000]]) {
  test(`public Node WASM (${kind} evaluator): ${budget ?? 'default 3000'} inline token budget preserves discovery and dispatch`, { timeout: 20000 }, async (t) => {
    const server = await startResponsesServer();
    t.after(() => server.close());
    let dispatches = 0;
    const agent = await Agent.create({
      model: 'gpt-6.1-sol',
      transport: Transport.openAi({ apiKey: 'synthetic-discovery-key', websocketUrl: server.url, websocketWarmup: true }),
      tools: {
        '3🧭-naïve.$lookup': { description: 'Unicode named lookup.', parameters: deferredSchema, handler({ key }) { return { key }; } },
        library__small: { description: 'Read a short entry.', parameters: { type: 'object', properties: {}, additionalProperties: false }, handler: () => 'small' },
        library__lookup: { description: largeDescription, parameters: deferredSchema,
          outputSchema: { type: 'object', properties: { key: { type: 'string' } }, required: ['key'] },
          handler({ key }) { dispatches++; return { key }; } },
      },
      inlineDocsTokenBudget: budget,
      codeEvaluator: await evaluator(kind),
    });
    t.after(() => agent.dispose());
    const scenario = (async () => {
      const socket = await server.nextConnection();
      const reader = messageReader(socket);
      const request = await reader.next();
      const exec = request.input.flatMap(item => item.tools ?? []).find(tool => tool.name === 'exec');
      assert.ok(exec, JSON.stringify(request));
      const description = exec.description;
      const effectiveBudget = budget ?? 3000;
      const inlineStart = description.indexOf('\n\n## library');
      const inline = inlineStart < 0 ? '' : description.slice(inlineStart);
      assert.ok(Buffer.byteLength(inline) <= effectiveBudget * 4, `${Buffer.byteLength(inline)} > ${effectiveBudget * 4}`);
      if (effectiveBudget < 6000) {
        assert.doesNotMatch(description, /### `library__lookup`/);
        assert.match(description, /describeTool/);
        assert.match(description, /Some deferred nested tools/);
      } else {
        assert.ok(description.includes(largeDescription), 'larger configured budget includes the complete UTF-8 description');
        assert.match(description, /library__lookup\(args:/);
        assert.match(description, /Required synthetic lookup key/);
      }
      sendWarmup(socket, 'discovery-warmup');
      await reader.next();
      sendCompleted(socket, 'discovery-tool', [{ type: 'custom_tool_call', call_id: 'discover-call', name: 'exec', input: `
        const match = searchTools('catalog', { limit: 1 })[0];
        const full = describeTool(match.callableName);
        let denied;
        try { await tools.private__erase({}); } catch (error) { denied = error.code; }
        text({ name: full.name, schema: full.inputSchema, length: full.description.length,
          namespace: describeNamespace('library').map(tool => tool.name), denied,
          result: await tools[match.callableName]({ key: 'synthetic-answer' }),
          unicode: describeTool('3🧭-naïve.$lookup').callableName,
          unicodeResult: await tools[describeTool('3🧭-naïve.$lookup').callableName]({ key: 'unicode-answer' }) });
      ` }]);
      const continuation = await reader.next();
      const output = continuation.input.find(item => item.type === 'custom_tool_call_output');
      assert.ok(output, JSON.stringify(continuation.input));
      const content = typeof output.output === "string" ? JSON.parse(output.output) : output.output;
      const text = content.find(item => item.type === 'input_text' && item.text.startsWith('{'))?.text;
      const report = JSON.parse(text);
      assert.equal(report.name, 'library__lookup');
      assert.deepEqual(report.schema, deferredSchema);
      assert.equal(report.length, largeDescription.length);
      assert.deepEqual(report.namespace, ['library__small', 'library__lookup']);
      assert.equal(report.denied, 'TOOL_NOT_AVAILABLE');
      assert.deepEqual(report.result, { key: 'synthetic-answer' });
      assert.equal(report.unicode, '___na_ve_$lookup');
      assert.deepEqual(report.unicodeResult, { key: 'unicode-answer' });
      assert.equal(dispatches, 1);
      console.log(JSON.stringify({ surface: 'public-node-wasm-websocket', evaluator: kind, budget: effectiveBudget,
        inlineUtf8Bytes: Buffer.byteLength(inline), omittedLookup: effectiveBudget < 6000,
        describedCharacters: report.length, denied: report.denied, dispatches, result: report.result }));
      sendFinal(socket, 'discovery-final', 'synthetic-answer');
    })();
    const result = await agent.turn.prompt({ input: 'Discover the permitted lookup and retrieve the synthetic answer.' }).result();
    assert.equal(result.finalMessage, 'synthetic-answer');
    await scenario;
  });
}

for (const budget of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
  test(`public Node Agent rejects invalid inlineDocsTokenBudget ${budget}`, async () => {
    await assert.rejects(Agent.create({
      transport: Transport.openAi({ apiKey: 'synthetic-discovery-key', websocketUrl: 'ws://127.0.0.1:1' }),
      inlineDocsTokenBudget: budget,
    }), /inlineDocsTokenBudget must be a non-negative safe integer/);
  });
}
