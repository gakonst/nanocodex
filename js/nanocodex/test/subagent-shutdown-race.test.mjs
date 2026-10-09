// Run after pnpm --filter nanocodex-vite build:wasm:
// node --test js/nanocodex/test/subagent-shutdown-race.test.mjs
// Public SDK operations and real Rust/WASM against synthetic Responses HTTP.
// The internal host lifecycle observer is needed because generic SDK clients
// do not expose release reasons; the managed journey covers durable SQL effects.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Agent, Subagents, Transport } from '../host/index.mjs';
import { codeEvaluator } from './quickjs-fixture.mjs';
import { createMemoryDurabilityStore } from '../runtime/durability-store.mjs';

test('explicit child close overlapping root shutdown releases only that child permanently', { timeout: 30_000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
  const trace = [], releases = [], bindings = new Map(), started = new Map();
  const errors = [];
  let callId = 0;
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      assert.equal(request.url, '/v1/responses');
      assert.equal(request.headers.authorization, 'Bearer synthetic-close-race');
      const definitions = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      assert.deepEqual(definitions.map(tool => tool.name).sort(), ['exec', 'wait']);
      trace.push({ type: 'model_request', model: body.model });
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: {
        id: `response-${++callId}`, status: 'completed',
        output: [{ type: 'custom_tool_call', call_id: `hold-${callId}`, name: 'exec', input: 'text(await tools.hold({}));' }],
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
      } })}\n\n`);
    } catch (error) { errors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const root = await Agent.create({
    module, model: 'gpt-6.1-sol', thinking: 'low', codeEvaluator,
    transport: Transport.openAi({ apiKey: 'synthetic-close-race', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }),
    tools: [{ name: 'hold', description: 'Wait for cancellation', parameters: { type: 'object', properties: {}, additionalProperties: false },
      handler(_input, context) {
        const id = String(context.subagent.agentId);
        trace.push({ type: 'tool_started', id });
        started.get(id).resolve();
        return new Promise(resolve => context.signal.addEventListener('abort', () => {
          trace.push({ type: 'tool_aborted', id });
          resolve('ABORTED');
        }, { once: true }));
      },
    }],
    [Symbol.for('nanocodex.browser.internalRuntime')]: { subagentSessions: {
      bind(sessionId, descriptor) {
        bindings.set(String(descriptor.agentId), sessionId);
        if (!started.has(String(descriptor.agentId))) started.set(String(descriptor.agentId), Promise.withResolvers());
      },
      release(sessionId, _context, options) {
        releases.push({ sessionId, detach: options?.detach === true });
      },
    } },
  });
  let shutdown;
  try {
    const explicit = await Subagents.spawn(root, { role: 'explicit close', task: 'Call hold.', outputSchema: { type: 'string' } });
    await started.get(String(explicit.agent_id)).promise;
    const detached = await Subagents.spawn(root, { role: 'root teardown', task: 'Call hold.', outputSchema: { type: 'string' } });
    await started.get(String(detached.agent_id)).promise;
    // Do not await close: root teardown must overlap its resource drain.
    const closing = Subagents.close(root, explicit.agent_id);
    shutdown = root.session.shutdown();
    const closed = await closing;
    await shutdown;
    assert.equal(closed.agents.find(agent => agent.agent_id === explicit.agent_id).status.state, 'closed');
    assert.deepEqual(releases.filter(row => row.sessionId === bindings.get(String(explicit.agent_id))), [
      { sessionId: bindings.get(String(explicit.agent_id)), detach: false },
    ]);
    assert.deepEqual(releases.filter(row => row.sessionId === bindings.get(String(detached.agent_id))), [
      { sessionId: bindings.get(String(detached.agent_id)), detach: true },
    ]);
    assert.deepEqual(errors, []);
  } finally {
    await (shutdown ?? root.session.shutdown());
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/subagent-shutdown-race/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('trace.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/subagent-shutdown-race.test.mjs',
      expected: 'overlapping explicit close releases its binding permanently once; sibling shutdown detaches once',
      releases, trace, errors,
    }, null, 2));
  }
});

test('durable shutdown retains completed children and explicit closes across reconstruction', { timeout: 30_000 }, async () => {
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const trace = [], errors = [];
  let calls = 0;
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      trace.push(body);
      const last = body.input.at(-1);
      const finish = last?.type === 'custom_tool_call_output';
      const definitions = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      assert.deepEqual(definitions.map(tool => tool.name).sort(), ['exec', 'wait']);
      if (!finish && calls >= 4) assert.match(JSON.stringify(body.input), /DURABLE_HISTORY_PROOF/);
      const output = finish
        ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'CHILD_DONE' }] }]
        : [{ type: 'custom_tool_call', call_id: `submit-${calls}`, name: 'exec',
          input: `text(await tools.submit_result(${JSON.stringify({ output: 'DURABLE_HISTORY_PROOF' })}));` }];
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `response-${++calls}`, status: 'completed', output,
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch (error) { errors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const durabilityId = 'durable-shutdown-journey';
  const options = { module, durabilityId, durability: createMemoryDurabilityStore(durabilityId),
    model: 'gpt-6.1-sol', thinking: 'low', codeEvaluator,
    transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }) };
  let root;
  const results = [];
  try {
    root = await Agent.create(options);
    const kept = await Subagents.spawn(root, { role: 'retained', task: 'Submit the requested result.', outputSchema: { type: 'string' } });
    const closed = await Subagents.spawn(root, { role: 'explicitly closed', task: 'Submit the requested result.', outputSchema: { type: 'string' } });
    const completed = { agents: [] };
    for (const id of [kept.agent_id, closed.agent_id]) {
      const result = await Subagents.wait(root, { agentIds: [id], timeoutMs: 5_000 });
      completed.agents.push(...result.agents);
    }
    assert.ok(completed.agents.every(agent => agent.status.state === 'completed'), JSON.stringify(completed));
    results.push(completed);
    await Subagents.close(root, closed.agent_id);
    await root.session.shutdown();
    // Reopening must observe shutdown's final durable state, not race an old writer.
    await new Promise(resolve => setImmediate(resolve));
    root = await Agent.create(options);
    const restored = await Subagents.list(root, { includeCompleted: true });
    results.push(restored);
    assert.deepEqual(restored.agents.find(agent => agent.agent_id === kept.agent_id).status,
      { state: 'completed', output: 'DURABLE_HISTORY_PROOF' });
    assert.equal(restored.agents.find(agent => agent.agent_id === closed.agent_id).status.state, 'closed');
    await Subagents.send(root, { agentId: kept.agent_id, purpose: 'delegate', message: 'Recall and submit your original result.' });
    const continued = await Subagents.wait(root, { agentIds: [kept.agent_id], timeoutMs: 5_000 });
    results.push(continued);
    assert.deepEqual(continued.agents[0].status, { state: 'completed', output: 'DURABLE_HISTORY_PROOF' });
    assert.deepEqual(errors, []);
  } finally {
    await root?.session.shutdown();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/subagent-shutdown-race/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('durable-reconstruction.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/subagent-shutdown-race.test.mjs',
      expected: 'same completed child and history survive shutdown; explicit close remains closed', results, trace, errors,
    }, null, 2));
  }
});

test('shutdown during every automatic child resume stops at the durable recovery budget', { timeout: 30_000 }, async () => {
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const trace = [], errors = [];
  let calls = 0, starts = 0, finishing = false, started = Promise.withResolvers();
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const last = body.input.at(-1);
      const resumes = JSON.stringify(body.input).split('runtime restarted while your previous turn was running').length - 1;
      trace.push({ type: 'model_request', finishing, last: last?.type, resumes });
      // Uncommitted held calls are intentionally absent; each committed resume prompt is retained.
      if (finishing) assert.equal(resumes, 3, 'delegated turn keeps the restored conversation history');
      const output = finishing && last?.type === 'custom_tool_call_output'
        ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'CHILD_DONE' }] }]
        : [{ type: 'custom_tool_call', call_id: `call-${calls}`, name: 'exec', input: finishing
          ? `text(await tools.submit_result(${JSON.stringify({ output: 'REUSED_AFTER_EXHAUSTION' })}));`
          : 'text(await tools.hold({}));' }];
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `response-${++calls}`, status: 'completed', output,
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch (error) { errors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const durabilityId = 'durable-resume-budget';
  const options = { module, durabilityId, durability: createMemoryDurabilityStore(durabilityId),
    model: 'gpt-6.1-sol', thinking: 'low', codeEvaluator,
    transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }),
    tools: [{ name: 'hold', description: 'Hold until runtime shutdown', parameters: { type: 'object', properties: {}, additionalProperties: false },
      handler(_input, context) {
        trace.push({ type: 'tool_started', start: ++starts });
        started.resolve();
        return new Promise(resolve => context.signal.addEventListener('abort', () => resolve('ABORTED'), { once: true }));
      } }] };
  const nextStart = async () => {
    await Promise.race([started.promise, new Promise((_, reject) => setTimeout(() => reject(new Error(`no hold start after ${starts}`)), 5_000))]);
    started = Promise.withResolvers();
  };
  const reopen = async () => {
    await root.session.shutdown();
    await new Promise(resolve => setImmediate(resolve));
    root = await Agent.create(options);
    const listed = await Subagents.list(root, { includeCompleted: true });
    trace.push({ type: 'restored', listed });
    return listed;
  };
  let root, child, exhausted, continued;
  try {
    root = await Agent.create(options);
    child = await Subagents.spawn(root, { role: 'held', task: 'Call hold.', outputSchema: { type: 'string' } });
    await nextStart();
    // Each teardown lands while the restored child is running again.
    for (let attempt = 1; attempt <= 3; attempt++) {
      await reopen();
      await nextStart();
    }
    const requestsBeforeExhaustion = calls;
    exhausted = await reopen();
    const status = exhausted.agents.find(agent => agent.agent_id === child.agent_id).status;
    assert.equal(status.state, 'failed', JSON.stringify(exhausted));
    assert.match(status.error, /subagent recovery exhausted/);
    await new Promise(resolve => setTimeout(resolve, 200));
    assert.equal(starts, 4, 'one original turn plus exactly three automatic resumes');
    assert.equal(calls, requestsBeforeExhaustion, 'exhausted restore dispatches no fourth resume');
    finishing = true;
    await Subagents.send(root, { agentId: child.agent_id, purpose: 'delegate', message: 'Submit the recovery result.' });
    continued = await Subagents.wait(root, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.deepEqual(continued.agents[0].status, { state: 'completed', output: 'REUSED_AFTER_EXHAUSTION' });
    assert.deepEqual(errors, []);
  } finally {
    await root?.session.shutdown();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/subagent-shutdown-race/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('resume-budget.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/subagent-shutdown-race.test.mjs',
      expected: 'held child resumes after each of three shutdowns, then restores failed (recovery exhausted) with no fourth dispatch and accepts an explicit delegation',
      starts, calls, exhausted, continued, trace, errors,
    }, null, 2));
  }
});

test('restarts during a progressing child turn do not exhaust the consecutive recovery budget', { timeout: 60_000 }, async () => {
  // Hosted regression (session 01a120c9, 2026-10-09): runtime resets every
  // minute or two interrupted long delegated turns that kept making progress
  // between resets; three resets in one turn failed every child with
  // "subagent recovery exhausted". Only resumes without committed progress
  // may consume the budget.
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const trace = [], errors = [];
  let calls = 0, steps = 0, holds = 0, finishing = false, held = Promise.withResolvers();
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const last = body.input.at(-1);
      const output = last?.type === 'custom_tool_call_output' ? JSON.stringify(last.output) : '';
      trace.push({ type: 'model_request', finishing, last: last?.type, output });
      const exec = input => [{ type: 'custom_tool_call', call_id: 'call-' + calls, name: 'exec', input }];
      const items = finishing
        ? (output.includes('accepted') ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'CHILD_DONE' }] }]
          : exec('text(await tools.submit_result(' + JSON.stringify({ output: 'PROGRESS_KEPT' }) + '));'))
        // Each resume completes one tool (committed progress), then holds the turn open.
        : output.includes('STEP_DONE') ? exec('text(await tools.hold({}));') : exec('text(await tools.step({}));');
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end('data: ' + JSON.stringify({ type: 'response.completed', response: { id: 'response-' + (++calls), status: 'completed', output: items,
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } }) + '\n\n');
    } catch (error) { errors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const durabilityId = 'durable-resume-progress';
  const empty = { type: 'object', properties: {}, additionalProperties: false };
  const options = { module, durabilityId, durability: createMemoryDurabilityStore(durabilityId),
    model: 'gpt-6.1-sol', thinking: 'low', codeEvaluator,
    transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: 'http://127.0.0.1:' + server.address().port + '/v1', stateless: true }),
    tools: [
      { name: 'step', description: 'Complete one unit of work', parameters: empty,
        handler() { trace.push({ type: 'step', step: ++steps }); return 'STEP_DONE'; } },
      { name: 'hold', description: 'Hold until runtime shutdown', parameters: empty,
        handler(_input, context) {
          trace.push({ type: 'hold', hold: ++holds });
          held.resolve();
          return new Promise(resolve => context.signal.addEventListener('abort', () => resolve('ABORTED'), { once: true }));
        } },
    ] };
  const nextHold = async () => {
    await Promise.race([held.promise, new Promise((_, reject) => setTimeout(() => reject(new Error('no hold after ' + holds)), 10_000))]);
    held = Promise.withResolvers();
  };
  let root, child, restored = [], completed;
  const reopen = async () => {
    await root.session.shutdown();
    await new Promise(resolve => setImmediate(resolve));
    root = await Agent.create(options);
    const listed = await Subagents.list(root, { includeCompleted: true });
    trace.push({ type: 'restored', listed });
    restored.push(listed.agents.find(agent => agent.agent_id === child.agent_id).status);
  };
  try {
    root = await Agent.create(options);
    child = await Subagents.spawn(root, { role: 'progressing', task: 'Call step, then hold.', outputSchema: { type: 'string' } });
    await nextHold();
    // Twice the budget: every reset lands after the resumed turn finished a tool.
    for (let restart = 1; restart <= 6; restart++) {
      await reopen();
      await nextHold();
    }
    assert.ok(restored.every(status => status.state !== 'failed'), JSON.stringify(restored));
    assert.equal(holds, 7, 'the original turn plus one resume per restart');
    assert.equal(steps, 7, 'every resume committed new work before the next restart');
    finishing = true;
    await reopen();
    completed = await Subagents.wait(root, { agentIds: [child.agent_id], timeoutMs: 10_000 });
    assert.deepEqual(completed.agents[0].status, { state: 'completed', output: 'PROGRESS_KEPT' });
    assert.deepEqual(errors, []);
  } finally {
    await root?.session.shutdown();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/subagent-shutdown-race/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('resume-progress.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/subagent-shutdown-race.test.mjs',
      expected: 'child that completes a tool after each of six restarts keeps resuming (never recovery exhausted) and completes its original turn',
      steps, holds, calls, restored, completed, trace, errors,
    }, null, 2));
  }
});

test('a failed final journal flush cannot let shutdown cleanup persist closed children', { timeout: 30_000 }, async () => {
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const errors = [], saves = [];
  let calls = 0, failNextJournal = false;
  const server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const output = body.input.at(-1)?.type === 'custom_tool_call_output'
        ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'CHILD_DONE' }] }]
        : [{ type: 'custom_tool_call', call_id: `submit-${calls}`, name: 'exec',
          input: `text(await tools.submit_result(${JSON.stringify({ output: 'SURVIVES_FAILED_FLUSH' })}));` }];
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `response-${++calls}`, status: 'completed', output,
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch (error) { errors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const durabilityId = 'durable-failed-final-flush';
  const memory = createMemoryDurabilityStore(durabilityId);
  // One synthetic storage outage on the subagent journal during shutdown.
  const durability = { ...memory, replace(selected, request) {
    const journal = selected.endsWith(':subagents');
    if (journal && failNextJournal) {
      failNextJournal = false;
      saves.push({ selected, outcome: 'synthetic outage' });
      throw new Error('synthetic subagent journal outage');
    }
    const result = memory.replace(selected, request);
    if (journal) saves.push({ selected, outcome: result.status });
    return result;
  } };
  const options = { module, durabilityId, durability, model: 'gpt-6.1-sol', thinking: 'low', codeEvaluator,
    transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }) };
  let root, restored, shutdownError;
  try {
    root = await Agent.create(options);
    const child = await Subagents.spawn(root, { role: 'retained', task: 'Submit the requested result.', outputSchema: { type: 'string' } });
    const completed = await Subagents.wait(root, { agentIds: [child.agent_id], timeoutMs: 5_000 });
    assert.equal(completed.agents[0].status.state, 'completed', JSON.stringify(completed));
    await new Promise(resolve => setTimeout(resolve, 50));
    failNextJournal = true;
    await root.session.shutdown().catch(error => { shutdownError = String(error); });
    // Let runtime release and any retried teardown finish before reopening.
    await new Promise(resolve => setTimeout(resolve, 100));
    root = await Agent.create(options);
    restored = await Subagents.list(root, { includeCompleted: true });
    assert.equal(failNextJournal, false, 'the shutdown flush hit the synthetic outage');
    assert.deepEqual(restored.agents.find(agent => agent.agent_id === child.agent_id).status,
      { state: 'completed', output: 'SURVIVES_FAILED_FLUSH' }, JSON.stringify({ restored, saves, shutdownError }));
    assert.deepEqual(errors, []);
  } finally {
    await root?.session.shutdown();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/subagent-shutdown-race/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('failed-final-flush.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/subagent-shutdown-race.test.mjs',
      expected: 'one failed final journal save during shutdown leaves the completed child completed after reconstruction',
      saves, shutdownError, restored, errors,
    }, null, 2));
  }
});
