// Public host SDK + real Rust/QuickJS WASM over HTTP. Only the model service is synthetic.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Agent, Transport, createQuickJsEvaluator } from '../host/index.mjs';
import asyncVariant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';
import { ToolRouter, toolMapSource, providerSource } from '../runtime/tool-router.mjs';

const parameters = { type: 'object', properties: {}, additionalProperties: false };
const deferred = { type: 'function', name: 'remote_echo', description: 'Discovered fixture tool', parameters, strict: false, defer_loading: true };
const definition = (name, body) => ({ type: 'custom_tool_call', name: 'exec', call_id: name, input: body });

test('code-only keeps workspace, discovery and canonical children nested across exec/wait', { timeout: 60_000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
  const evaluate = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant));
  const trace = [], effects = [], errors = [];
  let rootStep = 0, childStep = 0, id = 0;
  const router = new ToolRouter([toolMapSource('application', {
    fail: { description: 'Fail a nested fixture operation', parameters, handler() { throw new Error('NESTED_FAILURE_RECEIPT'); } },
    exec_command: { description: 'Synthetic workspace operation', parameters, handler() { effects.push('workspace'); return 'WORKSPACE_OK'; } },
    pause: { description: 'Wait briefly to test cell retention', parameters, async handler() { await new Promise(resolve => setTimeout(resolve, 100)); return 'PAUSE_OK'; } },
  }), providerSource('discovered', {
    definitions: () => [deferred],
    resolve: name => name === 'remote_echo' ? { name, handler() { effects.push('remote'); return 'REMOTE_OK'; } } : undefined,
  })]);
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      trace.push(body);
      const schemas = [...(body.tools ?? []), ...(body.input ?? []).filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      assert.deepEqual(schemas.map(tool => tool.name).sort(), ['exec', 'wait']);
      assert.ok(body.input.filter(item => item.type === 'tool_search_output').every(item => item.tools.length === 0), 'direct discovery never expands schemas');
      const history = JSON.stringify(body.input.filter(item => item.type !== 'additional_tools'));
      const child = !body.input.some(item => item.role === 'user' && JSON.stringify(item.content).includes('STRICT_ROOT_TASK'));
      let output;
      if (child) {
        output = childStep++ === 0 ? [definition('child-submit', 'text(await tools.submit_result({output:"CHILD_OK"}));')]
          : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Child submitted.' }] }];
      } else {
        switch (rootStep++) {
          case 0: output = [definition('root-discover', 'text(await tools.exec_command({})); try { await tools.fail({}); } catch (error) { text(error.message); } text(await tools.tool_search({query:"remote_echo"}));')]; break;
          case 1:
            assert.match(history, /WORKSPACE_OK/);
            assert.match(history, /NESTED_FAILURE_RECEIPT/);
            output = [definition('root-spawn', 'text(await tools.remote_echo({})); text(ALL_TOOLS.map(tool=>tool.name)); text(await tools.list_agents({})); const child=await tools.spawn_agent({role:"fixture",task:"STRICT_CHILD_TASK",harness:null,model:null,thinking:null,output_contract:{kind:"string"}}); store("child",child.agent_id); text(child);')]; break;
          case 2:
            assert.match(history, /REMOTE_OK/);
            assert.match(history, /spawn_agent/);
            output = [definition('root-yield', '// @exec: {"yield_time_ms": 0}\nawait tools.pause({}); text(await tools.wait_agent({agent_ids:[load("child")],timeout_ms:10000}));')]; break;
          case 3: {
            const receipt = body.input.findLast(item => item.type === 'custom_tool_call_output' && item.call_id === 'root-yield');
            const cell = JSON.stringify(receipt).match(/Script running with cell ID ([^\\"\s]+)/)?.[1];
            assert.ok(cell, JSON.stringify(receipt));
            output = [{ type: 'function_call', name: 'wait', call_id: 'root-wait', arguments: JSON.stringify({cell_id:cell}) }]; break;
          }
          case 4:
            assert.match(history, /CHILD_OK/);
            output = [{ type: 'function_call', name: 'exec_command', call_id: 'stale-direct', arguments: '{}' }]; break;
          case 5:
            assert.match(history, /requires Code Mode/);
            output = [{ type: 'tool_search_call', execution: 'client', call_id: 'stale-search', arguments: {query:'remote_echo'} }]; break;
          case 6:
            assert.deepEqual(body.input.findLast(item => item.type === 'tool_search_output')?.tools, []);
            output = [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'STRICT_OK' }] }]; break;
          default: throw new Error('unexpected root request');
        }
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `fixture-${++id}`, status: 'completed', output, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch(error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let agent, failure;
  try {
    agent = await Agent.create({ module,
      [Symbol.for("nanocodex.browser.internalRuntime")]: { traceTool: (_name, _context, run) => run() },
      codeEvaluator:evaluate, model:'gpt-6.1-sol', thinking:'low', tools:router,
      transport:Transport.openAi({ apiKey:'synthetic', apiBaseUrl:`http://127.0.0.1:${server.address().port}/v1`, stateless:true }) });
    const result = await agent.turn.prompt({input:'STRICT_ROOT_TASK'}).result();
    assert.equal(result.finalMessage,'STRICT_OK');
    assert.deepEqual(effects,['workspace','remote'], 'stale direct tool call cannot execute');
    assert.deepEqual(errors,[]);
  } catch(error) {failure=error;throw error;}
  finally {
    await agent?.dispose();
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/sdk-code-only/', import.meta.url);
    await mkdir(output,{recursive:true});
    await writeFile(new URL('journey.json',output),JSON.stringify({status:failure?'failed':'passed',error:failure?.stack,errors,effects,trace},null,2));
  }
});

test('code-only resume removes historical discovery schemas and preserves the transcript', { timeout: 30_000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
  const evaluate = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant));
  const trace = [], errors = [], effects = [];
  let step = 0, restoreMode;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      trace.push(body);
      let output;
      if (step === 0) {
        output = [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'LEGACY_HISTORY_OK' }] }];
      } else {
        const schemas = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
        assert.deepEqual(schemas.map(tool => tool.name).sort(), ['exec', 'wait']);
        const archived = body.input.find(item => item.id === 'at_legacy_fixture');
        assert.ok(archived, 'historical capability item remains in the transcript');
        assert.equal(archived.tools.length, 0);
        const searches = body.input.filter(item => item.type === 'tool_search_output');
        assert.equal(searches.length, 1, 'historical discovery receipt remains paired');
        assert.equal(searches[0].tools.length, 0, 'only strict resume removes historical discovery schemas');
        assert.deepEqual(body.input.find(item => item.type === 'function_call_output' && item.call_id === 'legacy-direct'), checkpoint.payload.conversation.history.find(item => item.type === 'function_call_output' && item.call_id === 'legacy-direct'), 'old tool result survives unchanged');
        assert.match(JSON.stringify(body.input), /KEEP_USER_HISTORY/);
        assert.match(JSON.stringify(body.input), /LEGACY_HISTORY_OK/);
        assert.equal(body.previous_response_id, undefined);
        output = [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'STRICT_RESUME_OK' }] }];
      }
      step += 1;
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `resume-${step}`, status: 'completed', output, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const options = toolMode => ({ module, toolMode, codeEvaluator: evaluate, model: 'gpt-6.1-sol', thinking: 'low',
    [Symbol.for('nanocodex.browser.internalRuntime')]: { subagentsEnabled: false },
    tools: new ToolRouter([providerSource('legacy-provider', {
      definitions: () => [deferred],
      resolve: name => name === 'remote_echo' ? { name, handler: () => { effects.push('remote'); return 'REMOTE_OK'; } } : undefined,
    })]),
    transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }),
  });
  let original, resumed, failure, checkpoint;
  try {
    original = await Agent.create(options('code-only'));
    const result = await original.turn.prompt({ input: 'KEEP_USER_HISTORY' }).result();
    checkpoint = JSON.parse(JSON.stringify(await result.checkpoint()));
    // Intentional archived protocol fixture: the Codex payload of a stored
    // checkpoint may contain direct-session items from older archives, which
    // must remain resumable. Only this fixture reaches into the native payload.
    const history = checkpoint.payload.conversation.history;
    history.push(
      { type: 'tool_search_call', execution: 'client', call_id: 'legacy-search', arguments: { query: 'remote_echo' } },
      { type: 'tool_search_output', call_id: 'legacy-search', execution: 'client', status: 'completed', tools: [deferred] },
      { type: 'function_call', name: 'remote_echo', call_id: 'legacy-direct', arguments: '{}' },
      { type: 'function_call_output', id: 'fco_legacy_fixture', call_id: 'legacy-direct', output: 'REMOTE_OK' },
    );
    await original.dispose(); original = undefined;
    // Stored checkpoints can contain capability items from older archived sessions.
    history.push({ type: 'additional_tools', id: 'at_legacy_fixture', role: 'developer', tools: [{ ...deferred, name: 'archived_tool' }] });
    for (restoreMode of ['code-only']) {
      resumed = await Agent.create({ ...options(restoreMode), resume: checkpoint });
      const restored = await resumed.turn.prompt({ input: 'Resume the archived session.' }).result();
      assert.equal(restored.finalMessage, 'STRICT_RESUME_OK');
      const saved = await restored.checkpoint();
      assert.equal(saved.payload.conversation.history.find(item => item.type === 'tool_search_output').tools.length, 0);
      await resumed.dispose(); resumed = undefined;
    }
    assert.equal(history.find(item => item.type === 'tool_search_output').tools.length, 1, 'resume does not mutate the supplied checkpoint');
    assert.deepEqual(effects, []);
    assert.equal(step, 2);
    assert.deepEqual(errors, []);
  } catch (error) { failure = error; throw error; }
  finally {
    await original?.dispose(); await resumed?.dispose();
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/sdk-code-only/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('resume.json', output), JSON.stringify({ status: failure ? 'failed' : 'passed', error: failure?.stack, errors, effects, checkpoint, trace }, null, 2));
  }
});

import { createMemoryDurabilityStore, exportDurabilityState, importDurabilityState } from '../runtime/durability-store.mjs';

for (const oldMode of ['code-only']) {
  test(`active ${oldMode} operation resumes after durable reopen`, { timeout: 30_000 }, async () => {
    const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
    const evaluate = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant));
    const entered = Promise.withResolvers(), trace = [], errors = [], effects = [];
    let upgraded = false;
    const server = createServer(async (request, response) => {
      try {
        const parts = []; for await (const part of request) parts.push(part);
        const body = JSON.parse(Buffer.concat(parts)); trace.push(body);
        const schemas = [...(body.tools ?? []), ...(body.input ?? []).filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
        let output;
        if (!upgraded) {
          assert.deepEqual(schemas.map(tool => tool.name).sort(), ['exec', 'wait']);
          entered.resolve(); return;
        } else {
          assert.deepEqual(schemas.map(tool => tool.name).sort(), ['exec', 'wait']);
          assert.ok(body.input.filter(item => item.type === 'tool_search_output').every(item => item.tools.length === 0));
          const history = JSON.stringify(body.input);
          assert.match(history, /PRESERVE_ACTIVE_USER_REQUEST/);
          output = history.includes('ACTIVE_WORKSPACE_OK')
            ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'ACTIVE_UPGRADE_OK' }] }]
            : [definition('active-nested-action', 'text(await tools.exec_command({}));')];
        }
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `active-${trace.length}`, status: 'completed', output, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
      } catch (error) { errors.push(error.stack); response.destroy(error); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const stateId = `codex-active-upgrade-${oldMode}`;
    const originalStore = createMemoryDurabilityStore(stateId);
    const options = (toolMode, durability) => ({ module, model: 'gpt-6.1-sol', thinking: 'low', toolMode, codeEvaluator: evaluate,
      durability, durabilityId: stateId,
      [Symbol.for('nanocodex.browser.internalRuntime')]: { subagentsEnabled: false },
      tools: new ToolRouter([toolMapSource('workspace', {
        exec_command: { description: 'Synthetic workspace operation', parameters, handler() { effects.push('workspace'); return 'ACTIVE_WORKSPACE_OK'; } },
      }), providerSource('remote', {
        definitions: () => [deferred], resolve: name => name === 'remote_echo' ? { name, handler: () => 'REMOTE_OK' } : undefined,
      })]),
      transport: Transport.openAi({ apiKey: 'synthetic', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true }),
    });
    const prompt = { input: 'PRESERVE_ACTIVE_USER_REQUEST', id: 'active-upgrade-operation' };
    let agent, failure;
    try {
      agent = await Agent.create(options(oldMode, originalStore));
      const turn = agent.turn.prompt(prompt), originalResult = turn.result().catch(error => error);
      await entered.promise;
      const archive = await exportDurabilityState(originalStore, stateId);
      await turn.cancel().catch(() => {}); await originalResult;
      await agent.dispose(); agent = undefined;
      const resumedStore = createMemoryDurabilityStore(stateId); await importDurabilityState(resumedStore, archive);
      upgraded = true;
      agent = await Agent.create(options('code-only', resumedStore));
      assert.equal((await agent.turn.prompt(prompt).result()).finalMessage, 'ACTIVE_UPGRADE_OK');
      assert.deepEqual(effects, ['workspace']); assert.deepEqual(errors, []);
    } catch (error) { failure = error; throw error; }
    finally {
      await agent?.dispose(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
      const output = new URL('../../../output/sdk-code-only/', import.meta.url);
      await mkdir(output, { recursive: true });
      await writeFile(new URL(`active-${oldMode}.json`, output), JSON.stringify({ status: failure ? 'failed' : 'passed', error: failure?.stack, errors, effects, trace }, null, 2));
    }
  });
}
