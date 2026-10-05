import assert from 'node:assert/strict';
import test from 'node:test';
import { createCodeRuntime } from 'nanocodex-tools/runtime/code-runtime';
import { ToolRouter } from '../runtime/tool-router.mjs';

async function evaluator(kind) {
  if (kind === 'native') return undefined;
  if (kind === 'quickjs') {
    const { default: variant } = await import('@jitl/quickjs-wasmfile-release-asyncify');
    const { newQuickJSAsyncWASMModuleFromVariant } = await import('quickjs-emscripten-core');
    const { createQuickJsEvaluator } = await import('../runtime/quickjs-evaluator.mjs');
    return createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(variant));
  }
  const { NodeWebWorker } = await import('./support/node-web-worker.mjs');
  const { createWorkerEvaluator } = await import('../runtime/worker-evaluator.mjs');
  return createWorkerEvaluator({ createWorker: () => new NodeWebWorker(new URL('../runtime/code-evaluator.worker.mjs', import.meta.url)) });
}

for (const kind of ['native', 'quickjs', 'worker']) {
  test(`${kind}: discover full admitted schemas, invoke normalized names, reject missing capabilities`, { timeout: 15000 }, async () => {
    let calls = 0;
    const schema = {
      type: 'object', properties: { value: { type: 'integer', description: 'value in the permitted range' } },
      required: ['value'], additionalProperties: false,
    };
    const outputSchema = { type: 'object', properties: { doubled: { type: 'integer' } }, required: ['doubled'] };
    const runtime = createCodeRuntime({
      'library.double': { description: 'Double a permitted number.', parameters: schema, outputSchema,
        handler({ value }) { calls++; return { doubled: value * 2 }; } },
      '3🧭-naïve.$lookup': { description: 'Read a Unicode named capability.', parameters: schema, handler({ value }) { calls++; return { doubled: value * 2 }; } },
      library__note: { description: 'Record a note.', parameters: { type: 'object' }, handler() { return 'note'; } },
    }, { evaluate: await evaluator(kind) });
    try {
      const result = JSON.parse(await runtime.executeCode(`
        const found = searchTools('LIBRARY double', { limit: 1 });
        const full = describeTool(found[0].callableName);
        let mutationRejected = false;
        try { full.inputSchema.properties.value.type = 'string'; } catch { mutationRejected = true; }
        let invalidLimit = false;
        try { searchTools('double', { limit: 101 }); } catch (error) { invalidLimit = error instanceof RangeError; }
        let denied;
        try { await tools.private__erase({}); } catch (error) { denied = { code: error.code, tool: error.tool }; }
        text({ found, full, mutationRejected, invalidLimit,
          namespace: describeNamespace('library').map(tool => tool.name),
          missing: describeTool('private__erase') === undefined,
          missingNamespace: describeNamespace('private'), denied,
          invoked: await tools[full.callableName]({ value: 21 }),
          unicode: describeTool('3🧭-naïve.$lookup').callableName,
          unicodeInvoked: await tools[describeTool('3🧭-naïve.$lookup').callableName]({ value: 2 }) });
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      const report = JSON.parse(result.output.at(-1).text);
      assert.deepEqual(report.found, [{ name: 'library.double', callableName: 'library_double', description: 'Double a permitted number.' }]);
      assert.deepEqual(report.full.inputSchema, schema);
      assert.deepEqual(report.full.outputSchema, outputSchema);
      assert.equal(report.full.inputSchema.properties.value.type, 'integer');
      assert.equal(report.invalidLimit, true);
      assert.deepEqual(report.namespace, ['library.double', 'library__note']);
      assert.equal(report.missing, true);
      assert.deepEqual(report.missingNamespace, []);
      assert.deepEqual(report.denied, { code: 'TOOL_NOT_AVAILABLE', tool: 'private__erase' });
      assert.deepEqual(report.invoked, { doubled: 42 });
      assert.equal(report.unicode, '___na_ve_$lookup');
      assert.deepEqual(report.unicodeInvoked, { doubled: 4 });
      assert.equal(calls, 2);
      assert.deepEqual(result.nested_calls.map(call => call.name), ['library.double', '3🧭-naïve.$lookup']);
      console.log(JSON.stringify({ runtime: kind, discovered: report.found, denied: report.denied, dispatched: calls, result: report.invoked }));
    } finally { runtime.reset(); }
  });

  test(`${kind}: admission pins catalog and handlers until the next cell`, { timeout: 15000 }, async () => {
    const definition = (name, description) => ({ type: 'function', name, description, strict: false,
      parameters: { type: 'object', properties: { value: { type: 'integer' } } } });
    let version = 1;
    let definitions = [definition('library__double', 'Version one')];
    const router = new ToolRouter([{
      id: 'synthetic-authorized-provider',
      definitions: () => definitions,
      resolve(name) {
        const pinned = version;
        return { name, handler: () => {
          version = 2;
          definitions[0].description = 'Version two';
          definitions[0].parameters.properties.value.type = 'string';
          if (!definitions.some(tool => tool.name === 'library__new')) definitions = [...definitions, definition('library__new', 'Newly authorized')];
          return pinned;
        } };
      },
    }]);
    const runtime = createCodeRuntime(router, { evaluate: await evaluator(kind) });
    try {
      const first = JSON.parse(await runtime.executeCode(`
        const before = describeTool('library__double');
        const invoked = await tools.library__double({ value: 1 });
        text({ invoked, description: describeTool('library__double').description,
          type: before.inputSchema.properties.value.type,
          fresh: searchTools('new').length, value: await tools.library__double({ value: 2 }) });
      `));
      assert.equal(first.success, true, JSON.stringify(first));
      assert.deepEqual(JSON.parse(first.output.at(-1).text), { invoked: 1, description: 'Version one', type: 'integer', fresh: 0, value: 1 });
      const second = JSON.parse(await runtime.executeCode(`
        text({ description: describeTool('library__double').description,
          type: describeTool('library__double').inputSchema.properties.value.type,
          fresh: searchTools('new').map(tool => tool.name), value: await tools.library__new({}) });
      `));
      assert.equal(second.success, true, JSON.stringify(second));
      assert.deepEqual(JSON.parse(second.output.at(-1).text), { description: 'Version two', type: 'string', fresh: ['library__new'], value: 2 });
    } finally { runtime.reset(); }
  });
}
