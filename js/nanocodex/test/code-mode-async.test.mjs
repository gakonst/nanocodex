import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AsyncLocalStorage } from 'node:async_hooks';
import { mkdir, writeFile } from 'node:fs/promises';
import { createCodeRuntime, toolResult } from '../../nanocodex-tools/runtime/code-runtime.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function textOf(output) {
  return typeof output === 'string' ? output : output.map(item => item.text ?? '').join('\n');
}
function scheduler(overrides = {}) {
  const delivered = deferred();
  const tasks = [];
  const trace = [];
  let admission = 0;
  return {
    trace, delivered, tasks,
    adapter: {
      enabled: () => true,
      async admit(context) {
        const jobId = `synthetic-job-${++admission}`;
        trace.push({ event: 'admitted', jobId, context });
        return { jobId, status: 'execute' };
      },
      async complete(context, receipt) {
        trace.push({ event: 'terminal-persisted', context, receipt });
        delivered.resolve(receipt);
      },
      retain(task) { tasks.push(task); },
      ...overrides,
    },
  };
}

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

for (const kind of ['native', 'quickjs', 'worker']) test(`${kind}: async exec returns admitted ID before dispatch, survives normal turn finish, and delivers real nested outcomes without wait`, { timeout: 5000 }, async () => {
  const started = deferred(), release = deferred();
  const jobs = scheduler();
  const journal = [];
  let identityCalls = 0;
  let signal;
  const runtime = createCodeRuntime({ write: {
    async handler(input, context) {
      signal = context.signal;
      assert.equal(jobs.trace[0].event, 'admitted');
      assert.equal(jobs.tasks.length, 1, 'host lifetime registered before dispatch');
      jobs.trace.push({ event: 'tool-dispatched', input, sessionId: context.sessionId, turnId: context.turnId });
      started.resolve();
      await release.promise;
      return toolResult('saved', { operation: input.operation }, {
        value: { actual: 'result' }, metadata: { request_id: 'synthetic-receipt', outcome: 'accepted' },
      });
    },
  } }, {
    asyncJobs: jobs.adapter,
    evaluate: await evaluator(kind),
    effectIdentity: async () => { identityCalls++; return { operationId: 'synthetic-origin', modelCallIndex: 3 }; },
    effectJournal: {
      async begin(context) { journal.push({ event: 'effect-admitted', context }); return { status: 'execute' }; },
      async complete(context, receipt) { journal.push({ event: 'effect-persisted', context, receipt }); },
    },
  });
  runtime.beginTurn('session');
  try {
    const admitted = JSON.parse(await runtime.executeCodeObserved('// @exec: {"yield_time_ms":120000}\ntext("before"); const result = await tools.write({operation:"once"}); text(result.actual); notify("done"); store("value", result.actual);', 'session', 'origin', 'model', 'turn-original'));
    assert.equal(admitted.job_id, 'synthetic-job-1');
    assert.equal(jobs.trace[0].context.operationId, 'synthetic-origin');
    assert.equal(jobs.trace[0].context.modelCallIndex, 3);
    assert.equal(admitted.cell.running, true);
    assert.equal(signal, undefined, 'receipt returns before even a fast guest dispatch');
    assert.equal(await runtime.nextCodeUpdate('session', 'origin'), null, 'exec observer already closed');
    runtime.finishTurn('session');
    runtime.beginTurn('session');
    await started.promise;
    assert.equal(signal.aborted, false);
    release.resolve();
    const terminal = await jobs.delivered.promise;
    await Promise.all(jobs.tasks);
    assert.equal(terminal.job_id, admitted.job_id);
    assert.equal(terminal.status, 'completed');
    assert.match(textOf(terminal.output), /before[\s\S]*result/);
    assert.equal(terminal.nested_calls.length, 1);
    assert.deepEqual(terminal.nested_calls[0].metadata, { request_id: 'synthetic-receipt', outcome: 'accepted' });
    assert.deepEqual(terminal.notifications, [{ call_id: 'origin', text: 'done' }]);
    assert.equal(identityCalls, 1, 'effect identity was pinned before originating turn ended');
    assert.equal(journal[1].context.turnId, 'turn-original');
    assert.equal(journal[1].context.operationId, 'synthetic-origin');
    const stored = JSON.parse(await runtime.executeCode('text(load("value"))', 'session'));
    assert.match(textOf(stored.output), /result/);
    const output = new URL(`../../../output/async-runtime-journey-${kind}.json`, import.meta.url);
    await mkdir(new URL('.', output), { recursive: true });
    await writeFile(output, JSON.stringify({ command: 'node --test js/nanocodex/test/code-mode-async.test.mjs', admitted, trace: jobs.trace, journal, stored }, null, 2));
  } finally { release.resolve(); runtime.reset(); }
});

test('async admission is fenced before dispatch and existing IDs never dispatch again', { timeout: 5000 }, async () => {
  const admission = deferred();
  let writes = 0;
  const jobs = scheduler({ admit: () => admission.promise });
  const runtime = createCodeRuntime({ write: { handler() { writes++; } } }, { asyncJobs: jobs.adapter });
  const pending = runtime.executeCodeObserved('await tools.write({})', 'session');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(writes, 0);
  admission.resolve({ jobId: 'already-retained', status: 'existing' });
  assert.equal(JSON.parse(await pending).job_id, 'already-retained');
  assert.equal(writes, 0);
  assert.equal(jobs.tasks.length, 0);
  runtime.reset();
});

test('explicit cancel aborts async guest and retains interrupted nested outcome', { timeout: 5000 }, async () => {
  const started = deferred(), release = deferred();
  const jobs = scheduler();
  let signal;
  const runtime = createCodeRuntime({ blocked: { async handler(_input, context) {
    signal = context.signal; started.resolve(); await release.promise; return 'late';
  } } }, { asyncJobs: jobs.adapter });
  runtime.beginTurn('session');
  await runtime.executeCodeObserved('await tools.blocked({}); text("must not appear")', 'session');
  await started.promise;
  runtime.finishTurn('session');
  runtime.beginTurn('session');
  runtime.cancelTurn('session');
  assert.equal(signal.aborted, true);
  const terminal = await jobs.delivered.promise;
  assert.equal(terminal.status, 'cancelled');
  assert.equal(terminal.success, false);
  assert.equal(terminal.nested_calls[0].structured_result.outcome, 'unknown');
  assert.doesNotMatch(textOf(terminal.output), /must not appear/);
  release.resolve(); await Promise.all(jobs.tasks); runtime.reset();
});

test('completion persistence failure rejects retained task and never reruns an effect', { timeout: 5000 }, async () => {
  let writes = 0;
  const jobs = scheduler({ async complete() { throw new Error('storage unavailable'); } });
  const runtime = createCodeRuntime({ write: { handler() { return ++writes; } } }, { asyncJobs: jobs.adapter });
  const admitted = JSON.parse(await runtime.executeCodeObserved('text(await tools.write({}))'));
  assert.equal(admitted.job_id, 'synthetic-job-1');
  await assert.rejects(jobs.tasks[0], /storage unavailable/);
  assert.equal(writes, 1);
  runtime.reset();
});

test('disabled mode keeps foreground completion and yield/wait behavior', { timeout: 5000 }, async () => {
  const release = deferred();
  const jobs = scheduler({ enabled: () => false, admit() { throw new Error('must not admit'); } });
  const runtime = createCodeRuntime({ blocked: { handler: () => release.promise } }, { asyncJobs: jobs.adapter });
  const fast = JSON.parse(await runtime.executeCodeObserved('text("foreground")'));
  assert.equal(fast.job_id, undefined);
  assert.match(textOf(fast.output), /Script completed[\s\S]*foreground/);
  const yielded = JSON.parse(await runtime.executeCodeObserved('// @exec: {"yield_time_ms":0}\ntext(await tools.blocked({}))', 'session', 'origin'));
  const id = textOf(yielded.output).match(/Script running with cell ID (\S+)/)[1];
  assert.equal(yielded.cell.running, true);
  release.resolve('actual');
  const completed = JSON.parse(await runtime.waitCodeObserved(JSON.stringify({ cell_id: id }), 'session'));
  assert.equal(completed.cell.running, false);
  assert.match(textOf(completed.output), /actual/);
  assert.equal(jobs.tasks.length, 0);
  runtime.reset();
});

test('admission storage failure dispatches nothing and surfaces host interruption', async () => {
  let writes = 0;
  const jobs = scheduler({ async admit() { throw new Error('admission unavailable'); } });
  const runtime = createCodeRuntime({ write: { handler() { writes++; } } }, { asyncJobs: jobs.adapter });
  await assert.rejects(runtime.executeCodeObserved('await tools.write({})'), { code: 'host_interrupted' });
  assert.equal(writes, 0);
  assert.equal(jobs.tasks.length, 0);
  runtime.reset();
});

test('cancellation during durable admission retains cancelled terminal without dispatch', { timeout: 5000 }, async () => {
  const admitted = deferred(), release = deferred();
  let writes = 0;
  const jobs = scheduler({ async admit() { admitted.resolve(); await release.promise; return { jobId: 'cancel-admission', status: 'execute' }; } });
  const runtime = createCodeRuntime({ write: { handler() { writes++; } } }, { asyncJobs: jobs.adapter });
  const pending = runtime.executeCodeObserved('await tools.write({})', 'cancel');
  await admitted.promise;
  runtime.cancel('cancel');
  release.resolve();
  assert.equal(JSON.parse(await pending).job_id, 'cancel-admission');
  assert.equal((await jobs.delivered.promise).status, 'cancelled');
  assert.equal(writes, 0);
  await Promise.all(jobs.tasks);
  runtime.reset();
});

test('nested effect retention failure remains an interrupted job with unknown write outcome', { timeout: 5000 }, async () => {
  let writes = 0;
  const jobs = scheduler();
  const runtime = createCodeRuntime({ write: { handler() { writes++; return 'saved'; } } }, {
    asyncJobs: jobs.adapter,
    effectJournal: { async begin() { return { status: 'execute' }; }, async complete() { throw new Error('lost effect persistence'); } },
  });
  await runtime.executeCodeObserved('try { await tools.write({}); } catch { text("must not hide interruption") }');
  const receipt = await jobs.delivered.promise;
  assert.equal(receipt.status, 'interrupted');
  assert.equal(receipt.success, false);
  assert.equal(receipt.nested_calls[0].structured_result.outcome, 'unknown');
  assert.doesNotMatch(textOf(receipt.output), /must not hide interruption/);
  assert.equal(writes, 1);
  await Promise.all(jobs.tasks);
  runtime.reset();
});

for (const kind of ['native', 'quickjs', 'worker']) test(`${kind}: scoped native controls survive admission, remain isolated, and journal actual receipts`, { timeout: 10000 }, async () => {
  const jobs = scheduler();
  const gate = deferred();
  const receipts = [];
  const calls = [];
  const runtime = createCodeRuntime({ cloud_read: { handler: () => 'cloud' } }, {
    evaluate: await evaluator(kind), asyncJobs: jobs.adapter,
    effectIdentity: (sessionId) => ({ operationId: `origin-${sessionId}`, modelCallIndex: 1 }),
    effectJournal: {
      begin: async () => ({ status: 'execute' }),
      complete: async (context, receipt) => receipts.push({ context, receipt }),
    },
  });
  const bridge = (session, revision) => ({
    definitions: [{ type: 'function', name: 'list_agents', description: 'Original native controls', parameters: { type: 'object' } },
      { type: 'custom', name: 'native_text', description: 'Native text output', format: { type: 'text' } }],
    async invoke(name, encoded, callId) {
      await gate.promise;
      if (name === "native_text") {
        assert.equal(JSON.parse(encoded), "echo");
        return JSON.stringify({ success: true, output: "native-text-result" });
      }
      calls.push({ name, input: JSON.parse(encoded), callId, session, revision });
      return JSON.stringify({ success: true, output: `native-${session}`, structured_result: { session, revision }, metadata: { origin: session } });
    },
  });
  try {
    await runtime.executeCodeObserved('text(ALL_TOOLS.map(t => t.name)); text(await tools.list_agents({})); text(await tools.cloud_read({})); text(await tools.native_text("echo"));', 'session-a', 'call-a', 'model', 'turn-a', bridge('session-a', 7));
    await runtime.executeCodeObserved('text(await tools.list_agents({}));', 'session-b', 'call-b', 'model', 'turn-b', bridge('session-b', 19));
    await runtime.executeCodeObserved('text(ALL_TOOLS.some(t => t.name === "list_agents")); try { await tools.list_agents({}); } catch (error) { text(error.code); }', 'session-c', 'call-c', 'model', 'turn-c');
    runtime.finishTurn('session-a'); runtime.beginTurn('session-a');
    gate.resolve();
    await Promise.all(jobs.tasks);
    assert.deepEqual(calls.map(call => [call.session, call.revision]).sort(), [['session-a', 7], ['session-b', 19]]);
    assert.equal(new Set(calls.map(call => call.callId)).size, 2);
    const terminals = jobs.trace.filter(item => item.event === 'terminal-persisted');
    const a = terminals.find(item => item.context.sessionId === 'session-a').receipt;
    const c = terminals.find(item => item.context.sessionId === 'session-c').receipt;
    assert.match(textOf(a.output), /list_agents/);
    assert.match(textOf(a.output), /cloud/);
    assert.match(textOf(c.output), /false/);
    assert.match(textOf(c.output), /TOOL_NOT_AVAILABLE/);
    assert.deepEqual(a.nested_calls[0].structured_result, { session: 'session-a', revision: 7 });
    assert.deepEqual(a.nested_calls[0].metadata, { origin: 'session-a' });
    assert.match(textOf(a.output), /native-text-result/);
    assert.equal(receipts.length, 4);
    await mkdir('output', { recursive: true });
    await writeFile(`output/async-native-tools-${kind}.json`, JSON.stringify({ calls, receipts, terminals }, null, 2));
  } finally { gate.resolve(); runtime.cancel('session-a'); runtime.cancel('session-b'); runtime.cancel('session-c'); }
});

test('legacy yielded cell remains waitable across ordinary finish and a later turn', async () => {
  const gate = deferred();
  const runtime = createCodeRuntime({ paused: { handler: async () => { await gate.promise; return 'legacy-result'; } } });
  runtime.beginTurn('legacy');
  const admitted = JSON.parse(await runtime.executeCodeObserved('// @exec: {"yield_time_ms":1}\ntext(await tools.paused({}));', 'legacy', 'origin'));
  const id = textOf(admitted.output).match(/cell ID ([^\s]+)/)[1];
  runtime.finishTurn('legacy');
  runtime.beginTurn('legacy');
  gate.resolve();
  const done = JSON.parse(await runtime.waitCodeObserved(JSON.stringify({ cell_id: id }), 'legacy', 'wait-final'));
  assert.equal(done.success, true);
  assert.match(textOf(done.output), /legacy-result/);
});

for (const kind of ['native', 'quickjs', 'worker']) test(`${kind}: authority scoping survives failed tracing and fences revoked dispatch`, { timeout: 5000 }, async () => {
  const scope = new AsyncLocalStorage();
  let revoked = false, calls = 0, modelCallIndex = 0, traces = 0;
  const jobs = scheduler({ run(context, invoke) {
    if (revoked) throw new Error('origin revoked');
    return scope.run(context.jobId, invoke);
  } });
  const runtime = createCodeRuntime({ effect: { handler() {
    calls++;
    assert.equal(scope.getStore(), 'synthetic-job-1');
    return 'scoped-effect';
  } } }, {
    asyncJobs: jobs.adapter, evaluate: await evaluator(kind),
    traceTool() { traces++; throw new Error('diagnostic sink unavailable'); },
    effectIdentity: async () => ({ operationId: 'same-origin', modelCallIndex: ++modelCallIndex }),
    effectJournal: { begin: async () => ({ status: 'execute' }), complete: async () => {} },
  });
  try {
    await runtime.executeCodeObserved('text(await tools.effect({}));', 'session', 'reused-call-id', 'model', 'turn');
    await Promise.all(jobs.tasks);
    revoked = true;
    await runtime.executeCodeObserved('text(await tools.effect({}));', 'session', 'reused-call-id', 'model', 'turn');
    await Promise.all(jobs.tasks);
    const admissions = jobs.trace.filter(item => item.event === 'admitted');
    assert.deepEqual(admissions.map(item => item.context.modelCallIndex), [1, 2]);
    assert.equal(calls, 1);
    assert.equal(traces, 1);
    const terminals = jobs.trace.filter(item => item.event === 'terminal-persisted');
    assert.equal(terminals[0].receipt.success, true);
    assert.equal(terminals[1].receipt.success, false);
    assert.match(textOf(terminals[1].receipt.output), /origin revoked/);
  } finally { runtime.cancel('session'); }
});
