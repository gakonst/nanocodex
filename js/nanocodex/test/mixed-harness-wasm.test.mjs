// Reproduce after pnpm --filter nanocodex-vite build:wasm:
// node --test js/nanocodex/test/mixed-harness-wasm.test.mjs
// Actual public host SDK, generated Rust WASM and one shared task tree. Only
// external Responses/Messages HTTP is synthetic. Evidence stays in output/.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Agent, Subagents, Transport, createQuickJsEvaluator } from '../host/index.mjs';
import { Agent as NodeAgent } from '../node/index.mjs';
import asyncVariant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

function messages(blocks, stop = 'end_turn') {
  const frames = [{ type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'fixture', content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((content_block, index) => frames.push({ type: 'content_block_start', index, content_block }, { type: 'content_block_stop', index }));
  frames.push({ type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } }, { type: 'message_stop' });
  return frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join('');
}

async function within(operation, label) {
  let timer;
  try {
    return await Promise.race([operation, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), 5000);
    })]);
  } finally { clearTimeout(timer); }
}

test('public SDK shares canonical children across both native harness families', { timeout: 60_000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
  const trace = [], effects = [], events = [], fixtureErrors = [], rejectedBatches = [];
  let responseId = 0;
  let blockedCallIssued = false;
  let codeCalls = 0;
  let blockedStarted;
  const started = new Promise(resolve => { blockedStarted = resolve; });
  let blockedAborted;
  const aborted = new Promise(resolve => { blockedAborted = resolve; });
  let blockedContext;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const claude = request.url === '/v1/messages';
      assert.equal(request.url, claude ? '/v1/messages' : '/v1/responses');
      assert.equal(claude ? request.headers['x-api-key'] : request.headers.authorization, claude ? 'synthetic-claude' : 'Bearer synthetic-codex', 'each family uses its explicitly authorized credential');
      trace.push({ path: request.url, model: body.model, familyAuthMatched: true, body });
      const history = claude ? body.messages : body.input;
      const encoded = JSON.stringify(history.filter(item => item.type !== 'additional_tools'));
      const submitted = history.some(item => item.type === 'function_call' && item.name === 'submit_result'
        || Array.isArray(item.content) && item.content.some(block => block.type === 'tool_use' && block.name === 'submit_result'));
      const block = claude && encoded.includes('BLOCK_UNTIL_INTERRUPT') && !blockedCallIssued;
      if (block) blockedCallIssued = true;
      const tool = block ? 'await_abort' : !encoded.includes('MIXED_EFFECT_RECEIPT') ? 'proof' : !submitted ? 'submit_result' : undefined;
      const args = tool === 'proof' ? {} : { output: { ok: true, model: body.model } };
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      if (claude) {
        response.end(messages(tool ? [{ type: 'tool_use', id: `${tool}-${++responseId}`, name: tool, input: args }] : [{ type: 'text', text: 'CHILD_COMPLETE' }], tool ? 'tool_use' : 'end_turn'));
      } else {
        const definitions = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
        const definition = tool && definitions.find(def => def.name === tool || def.description?.startsWith(`${tool}\n`));
        const exec = tool === 'proof' && !definition && definitions.find(def => def.name === 'exec');
        if (tool) assert.ok(definition || exec, `${tool} is an actual declared canonical/native tool or Code Mode capability`);
        if (exec) codeCalls++;
        const output = exec ? [{ type: 'custom_tool_call', call_id: `proof-code-${++responseId}`, name: exec.name, input: 'text(await tools.proof({}));' }]
          : tool ? [{ type: 'function_call', call_id: `${tool}-${++responseId}`, name: definition.name, arguments: JSON.stringify(args) }]
          : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'CHILD_COMPLETE' }] }];
        response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `response-${++responseId}`, status: 'completed', output, usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } })}\n\n`);
      }
    } catch (error) { fixtureErrors.push(String(error)); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const proof = { name: 'proof', description: 'Commit one synthetic effect', parameters: { type: 'object', properties: {}, additionalProperties: false }, handler(_input, context) {
    assert.ok(context.subagent, 'child identity reaches the actual host handler');
    assert.equal(context.signal.aborted, false, 'new and recovered handlers have live abort signals');
    effects.push({ model: context.model, sessionId: context.sessionId, subagent: context.subagent, parentCallId: context.parentCallId });
    return 'MIXED_EFFECT_RECEIPT';
  } };
  const codex = { module, model: 'gpt-6.1-sol', thinking: 'low', toolMode: 'direct', tools: [proof], transport: Transport.openAi({ apiKey: 'synthetic-codex', apiBaseUrl: `${base}/v1`, stateless: true }) };
  const claude = { module, model: 'claude-sonnet-4-6', thinking: 'low', endpoint: `${base}/v1/messages`, auth: { apiKey: 'synthetic-claude' }, tools: [{ name: proof.name, description: proof.description, inputSchema: proof.parameters, handler: proof.handler }, {
    name: 'await_abort', description: 'Hold one synthetic effect until cancellation', handler(_input, context) {
      assert.ok(context.subagent, 'interrupted handler receives child identity');
      assert.equal(context.signal.aborted, false);
      blockedContext = context;
      blockedStarted();
      return new Promise(resolve => context.signal.addEventListener('abort', () => {
        blockedAborted();
        resolve('BLOCK_ABORTED');
      }, { once: true }));
    },
  }] };
  const roots = [];
  try {
    roots.push(await Agent.create({ ...codex, harnesses: { claude } }));
    roots.push(await Agent.create({ ...claude, harness: 'claude', subagents: {}, harnesses: { codex } }));
    const beforeMissingEvaluator = trace.length;
    await assert.rejects(Agent.create({ ...claude, harness: 'claude', subagents: {}, harnesses: { codex: { ...codex, toolMode: 'code' } } }), /explicit codeEvaluator/);
    assert.equal(trace.length, beforeMissingEvaluator, 'missing host Code Mode evaluator fails before provider dispatch');
    const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);
    roots.push(await Agent.create({ ...claude, harness: 'claude', subagents: {}, harnesses: { codex: { ...codex, toolMode: 'code', codeEvaluator: createQuickJsEvaluator(quickJs) } } }));
    roots.push(await NodeAgent.create({ ...claude, module: undefined, harness: 'claude', subagents: {}, harnesses: { codex: { ...codex, toolMode: 'code' } } }));
    roots.push(await Agent.create({ ...claude, model: 'claude-proxy-fixture', thinking: undefined, harness: 'claude', subagents: {} }));
    const batchTask = { role: 'batch fixture', task: 'Perform proof once, then submit the typed result.', outputSchema: { type: 'object' } };
    for (const override of [{ harness: 'claude' }, { harness: 'codex' }, { model: 'sol' }, { thinking: 'low' }]) {
      const before = trace.length;
      await assert.rejects(Subagents.spawnMany(roots[0], [batchTask, { ...batchTask, ...override }]), /batch.*overrides/);
      assert.equal(trace.length, before, 'unsupported batch selection fails before any provider dispatch');
      const admittedChildren = (await Subagents.list(roots[0], { includeCompleted: true })).agents.length;
      assert.equal(admittedChildren, 0, 'the entire rejected batch leaves no admitted child');
      rejectedBatches.push({ override, providerRequests: trace.length - before, admittedChildren });
    }
    for (const [index, harness, model] of [[0, 'claude', 'claude-opus-5-5'], [1, 'codex', 'sol'], [2, 'codex', 'sol'], [3, 'codex', 'sol'], [4, undefined, undefined]]) {
      const root = roots[index], watcher = root.events.watch();
      const off = watcher.onEvent(event => events.push(event));
      const spawned = await Subagents.spawn(root, { role: 'fixture specialist', task: 'Perform proof once, then submit the typed result.', harness, model, outputSchema: { type: 'object', properties: { ok: { type: 'boolean' }, model: { type: 'string' } }, required: ['ok', 'model'], additionalProperties: false } });
      const report = await Subagents.wait(root, { agentIds: [spawned.agent_id], timeoutMs: 10_000 });
      assert.equal(report.timed_out, false, JSON.stringify(report));
      assert.equal(report.agents[0].status.state, 'completed', JSON.stringify(report));
      assert.equal(report.agents[0].status.output.ok, true);
      const expectedHarness = index === 0 || index === 4 ? 'claude' : 'codex';
      const expectedModel = index === 0 ? 'claude-opus-5-5' : index === 4 ? 'claude-proxy-fixture' : 'gpt-6.1-sol';
      const directory = (await Subagents.list(root, { includeCompleted: true })).agents;
      assert.equal(directory.length, 1);
      for (const actual of [spawned, report.agents[0], directory[0], effects.at(-1).subagent]) {
        assert.equal(actual.harness, expectedHarness, 'resolved child harness is consistent across public receipts and actual host effects');
        assert.equal(actual.model, expectedModel, 'resolved native model survives aliases and arbitrary explicit native IDs');
      }
      await Subagents.close(root, spawned.agent_id);
      off(); watcher.off();
    }
    assert.equal(effects.length, 5, 'one real host effect per child');
    assert.equal(effects[0].model, 'claude-opus-5-5', 'Codex dispatches the explicitly selected Opus child');
    assert.equal(effects[4].model, 'claude-proxy-fixture', 'native models inherit without catalog coercion');
    assert.equal(codeCalls, 2, 'host explicit evaluator and Node default evaluator execute actual Code Mode');
    for (const effect of effects.slice(2, 4)) assert.match(effect.parentCallId, /^proof-code-/, 'the actual host effect belongs to the Code Mode cell');
    assert.deepEqual(new Set(trace.map(row => row.path)), new Set(['/v1/responses', '/v1/messages']));
    const modelOnly = await Subagents.spawn(roots[1], { role: 'model fixture', task: 'Perform proof once, then submit the typed result.', model: 'sonnet', outputSchema: { type: 'object' } });
    const modelReport = await Subagents.wait(roots[1], { agentIds: [modelOnly.agent_id], timeoutMs: 10_000 });
    assert.equal(modelReport.timed_out, false);
    assert.equal(modelReport.agents[0].status.state, 'completed', JSON.stringify(modelReport));
    assert.equal(modelReport.agents[0].status.output.model, 'claude-sonnet-5-5', 'model-only selection inherits the Claude family');
    assert.equal(effects[5].model, 'claude-sonnet-5-5');
    await Subagents.close(roots[1], modelOnly.agent_id);
    const before = trace.length;
    await assert.rejects(Subagents.spawn(roots[0], { role: 'invalid', task: 'wrong family', harness: 'claude', model: 'sol', outputSchema: { type: 'string' } }), /model|harness/);
    assert.equal(trace.length, before, 'invalid family/model fails before dispatch');

    const root = roots[0];
    const child = await Subagents.spawn(root, { role: 'interrupt fixture', task: 'BLOCK_UNTIL_INTERRUPT: call await_abort, then submit the typed result.', harness: 'claude', model: 'claude-opus-5-5', thinking: 'low', outputSchema: { type: 'object', properties: { ok: { type: 'boolean' }, model: { type: 'string' } }, required: ['ok', 'model'], additionalProperties: false } });
    await within(started, 'the child handler to start');
    const interrupted = await within(Subagents.interrupt(root, child.agent_id), 'native child interruption');
    assert.equal(interrupted.agents[0].status.state, 'interrupted', JSON.stringify(interrupted));
    assert.equal(blockedContext.signal.aborted, true, 'native interrupt aborts the pending JavaScript handler before reporting inactive');
    await within(aborted, 'the child handler abort signal');
    await Subagents.send(root, { agentId: child.agent_id, purpose: 'delegate', message: 'Perform proof once, then submit the typed result.' });
    const recovered = await Subagents.wait(root, { agentIds: [child.agent_id], timeoutMs: 10_000 });
    assert.equal(recovered.timed_out, false, JSON.stringify(recovered));
    assert.equal(recovered.agents[0].status.state, 'completed', JSON.stringify(recovered));
    assert.equal(recovered.agents[0].status.output.ok, true);
    assert.equal(effects.length, 7, 'recovered child executes exactly one fresh proof');
    assert.equal(effects[6].model, 'claude-opus-5-5', 'interruption and resume preserve the selected Opus model');
    assert.equal(effects[6].sessionId, blockedContext.sessionId, 'recovery preserves the child session');
    assert.equal(effects[6].subagent.agentId, blockedContext.subagent.agentId, 'recovery preserves canonical child identity');
    await Subagents.close(root, child.agent_id);
  } finally {
    await Promise.all(roots.map(root => root.session.shutdown()));
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/mixed-harness-wasm/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('trace.json', output), JSON.stringify({ command: 'node --test js/nanocodex/test/mixed-harness-wasm.test.mjs', expected: 'four mixed children, one inherited native model and one model-only Claude selection; host and Node Code Mode; interrupt aborts handler before inactive; same identity on recovery; seven proof effects; invalid family and batch overrides never dispatched', observed: { codeCalls, blockedCallIssued, blockedSignalAborted: blockedContext?.signal.aborted }, fixtureErrors, rejectedBatches, trace, effects, events }, null, 2));
  }
});
