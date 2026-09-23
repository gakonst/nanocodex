import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createCodeRuntime } from '../runtime/code-runtime.mjs';

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
  for (const [ending, suffix, success] of [
    ['return', '', true],
    ['throw', 'throw new Error("guest failure");', false],
    ['exit', 'exit();', true],
  ]) test(`${kind}: ${ending} closes started calls once without waiting for an abort-ignoring handler`, { timeout: 10000 }, async () => {
    let release;
    const blocked = new Promise(resolve => { release = resolve; });
    const updates = [];
    let handlerStarted = false;
    const runtime = createCodeRuntime({
      blocked: {
        supportsParallelToolCalls: true,
        async handler() { handlerStarted = true; await blocked; return 'late result'; },
      },
      started: { supportsParallelToolCalls: true, handler() { return handlerStarted; } },
    }, { evaluate: await evaluator(kind) });
    try {
      // Wait for actual host dispatch without depending on engine scheduling.
      const source = `void tools.blocked({}); while (!await tools.started({})) {} ${suffix}`;
      const result = JSON.parse(await runtime.executeCode(source, 'lifecycle', `root-${ending}`, update => updates.push(structuredClone(update))));
      assert.equal(result.success, success, JSON.stringify(result));
      const starts = updates.filter(u => u.type === 'nested_call_started');
      const completions = updates.filter(u => u.type === 'nested_call_completed');
      assert.ok(starts.some(u => u.name === 'blocked'));
      for (const start of starts) assert.equal(completions.filter(u => u.call.call_id === start.call_id).length, 1);
      const receipt = completions.find(u => u.call.name === 'blocked').call;
      assert.equal(receipt.success, false);
      assert.equal(receipt.structured_result.code, 'CODE_MODE_CALL_INTERRUPTED');
      assert.equal(receipt.structured_result.outcome, 'unknown');
      assert.deepEqual(result.nested_calls.find(c => c.name === 'blocked'), receipt);
      const snapshot = JSON.stringify(updates);
      release();
      await new Promise(resolve => setTimeout(resolve, 20));
      assert.equal(JSON.stringify(updates), snapshot, 'late results must not duplicate or mutate terminal receipts');
    } finally { release(); runtime.reset(); }
  });
}

for (const kind of ['native', 'quickjs', 'worker']) {
  test(`${kind}: screenshot batch keeps both started siblings when third tool is unavailable`, { timeout: 10000 }, async () => {
    let started = 0;
    let release;
    const barrier = new Promise(resolve => { release = resolve; });
    const updates = [];
    const runtime = createCodeRuntime({
      read: {
        supportsParallelToolCalls: true,
        async handler({ id }) {
          if (++started === 2) release();
          await barrier;
          return id;
        },
      },
    }, { evaluate: await evaluator(kind) });
    try {
      const result = JSON.parse(await runtime.executeCode(`
        const results = await Promise.allSettled([
          tools.read({id:1}), tools.read({id:2}), tools.list_agents({include_self:true})
        ]);
        text(results.map(r => r.status === 'fulfilled'
          ? {status:r.status,value:r.value}
          : {status:r.status,code:r.reason.code,tool:r.reason.tool}));
      `, 'batch', 'batch', update => updates.push(structuredClone(update))));
      assert.equal(result.success, true, JSON.stringify(result));
      assert.equal(started, 2);
      assert.deepEqual(JSON.parse(result.output.at(-1).text), [
        {status:'fulfilled',value:1}, {status:'fulfilled',value:2},
        {status:'rejected',code:'TOOL_NOT_AVAILABLE',tool:'list_agents'},
      ]);
      assert.equal(result.nested_calls.length, 2);
      assert.ok(result.nested_calls.every(call => call.success));
      assert.equal(updates.filter(u => u.type === 'nested_call_completed').length, 2);
    } finally { release(); runtime.reset(); }
  });

  test(`${kind}: completed tools keep their successful receipt when guest later throws`, async () => {
    const updates = [];
    const runtime = createCodeRuntime({ done: { handler() { return 'done'; } } }, { evaluate: await evaluator(kind) });
    try {
      const result = JSON.parse(await runtime.executeCode(`await tools.done({}); throw new Error('later');`,
        'completed', 'completed', update => updates.push(structuredClone(update))));
      assert.equal(result.success, false);
      assert.equal(result.nested_calls.length, 1);
      assert.equal(result.nested_calls[0].success, true);
      assert.equal(updates.filter(u => u.type === 'nested_call_completed').length, 1);
    } finally { runtime.reset(); }
  });
}

async function observe(runtime, promise, session, call) {
  const updates = [];
  for (;;) {
    const update = await runtime.nextCodeUpdate(session, call);
    if (update === null) break;
    updates.push(JSON.parse(update));
  }
  return { ...JSON.parse(await promise), updates };
}

for (const kind of ['native', 'quickjs', 'worker']) {
  test(`${kind}: terminating a yielded cell closes its outstanding tool receipt`, { timeout: 10000 }, async () => {
    let release;
    let started = false;
    const blocked = new Promise(resolve => { release = resolve; });
    const runtime = createCodeRuntime({
      blocked: { supportsParallelToolCalls: true, async handler() { started = true; await blocked; return 'late'; } },
      started: { supportsParallelToolCalls: true, handler() { return started; } },
    }, { evaluate: await evaluator(kind) });
    try {
      const initial = await observe(runtime, runtime.executeCodeObserved(
        'void tools.blocked({}); while (!await tools.started({})) {} yield_control(); await new Promise(() => {});',
        'terminate', 'exec'), 'terminate', 'exec');
      const text = typeof initial.output === 'string' ? initial.output : initial.output.map(item => item.text ?? '').join('');
      const cell = text.match(/cell ID ([^\s]+)/)?.[1];
      assert.ok(cell, text);
      const stopped = await observe(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cell, terminate: true }),
        'terminate', 'wait'), 'terminate', 'wait');
      const start = initial.updates.find(u => u.type === 'nested_call_started' && u.name === 'blocked');
      const terminal = stopped.updates.filter(u => u.type === 'nested_call_completed' && u.call.call_id === start.call_id);
      assert.equal(terminal.length, 1);
      assert.equal(terminal[0].call.structured_result.code, 'CODE_MODE_CALL_INTERRUPTED');
      assert.equal(stopped.cell.running, false);
      assert.deepEqual(stopped.nested_calls.find(c => c.call_id === start.call_id), terminal[0].call);
    } finally { release(); runtime.reset(); }
  });
}

test('native: a late continuation cannot create a ghost completion after its cell closes', async () => {
  let release;
  let continued;
  const barrier = new Promise(resolve => { release = resolve; });
  const continuation = new Promise(resolve => { continued = resolve; });
  const updates = [];
  let fastCalls = 0;
  let slowStarted = false;
  const runtime = createCodeRuntime({
    slow: { supportsParallelToolCalls: true, async handler() { slowStarted = true; await barrier; return 'late'; } },
    started: { supportsParallelToolCalls: true, handler() { return slowStarted; } },
    fast: { handler() { fastCalls++; return 'unexpected'; } },
  }, { console: { log: () => continued() } });
  try {
    const result = JSON.parse(await runtime.executeCode(`
      void (async () => {
        try { await tools.slow({}); await tools.fast({}); }
        finally { console.log('continuation finished'); }
      })().catch(() => {});
      while (!await tools.started({})) {}
    `, 'late', 'late', update => updates.push(structuredClone(update))));
    assert.equal(result.success, true);
    const snapshot = JSON.stringify(updates);
    release();
    await continuation;
    assert.equal(fastCalls, 0);
    assert.equal(JSON.stringify(updates), snapshot);
  } finally { release(); runtime.reset(); }
});
