// Public Claude SDK, actual Rust WASM and QuickJS; only Messages HTTP is synthetic.
// Rebuild first: pnpm --filter nanocodex-vite build:wasm
// Run: node --test js/nanocodex/test/claude-code-only-wasm.test.mjs
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Claude, createQuickJsEvaluator } from '../host/index.mjs';
import asyncVariant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

function messages(block, stop = 'tool_use') {
  return [
    { type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'fixture', content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: block },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(frame => `data: ${JSON.stringify(frame)}\n\n`).join('');
}
const exec = (id, code) => ({ type: 'tool_use', id, name: 'exec', input: { code } });
const final = text => ({ type: 'text', text });
async function fixture(t, select) {
  const trace = [], errors = [];
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); trace.push(body);
      assert.deepEqual(body.tools.map(tool => tool.name).sort(), ['exec', 'wait']);
      const block = await select(body);
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(messages(block, block.type === 'text' ? 'end_turn' : 'tool_use'));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  return { trace, errors, options: {
    endpoint: `http://127.0.0.1:${server.address().port}/v1/messages`,
    model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic' },
    module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)),
    codeEvaluator: createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant)),
  } };
}
async function evidence(name, data) {
  const output = new URL('../../../output/claude-code-only/', import.meta.url);
  await mkdir(output, { recursive: true });
  await writeFile(new URL(`${name}.json`, output), JSON.stringify(data, null, 2));
}

test('Claude code-only nests native tools and canonical children, resumes cells, and rejects stale direct calls', { timeout: 60_000 }, async t => {
  let rootStep = 0, childStep = 0, stale = false;
  const effects = [];
  const f = await fixture(t, body => {
    const history = JSON.stringify(body.messages);
    const child = !history.includes('STRICT_CLAUDE_ROOT');
    if (child) return childStep++ === 0
      ? exec('child-submit', 'text(await tools.submit_result({output:"CHILD_OK"}));') : final('Child submitted.');
    if (stale) {
      const denial = body.messages.at(-1).content.find?.(block => block.tool_use_id === 'stale-direct');
      if (!denial) return { type: 'tool_use', id: 'stale-direct', name: 'Read', input: {} };
      assert.equal(denial.is_error, true); assert.doesNotMatch(JSON.stringify(denial.content), /Read/);
      return final('STALE_DIRECT_DENIED');
    }
    switch (rootStep++) {
      case 0: return exec('native-and-spawn', 'text(await tools.Read({})); text(ALL_TOOLS.map(tool=>tool.name)); const child=await tools.spawn_agent({role:"fixture",task:"STRICT_CHILD",harness:null,model:null,thinking:null,output_contract:{kind:"string"}}); store("child",child.agent_id); text(child);');
      case 1:
        assert.match(history, /READ_OK/); assert.match(history, /spawn_agent/);
        return exec('yield-cell', '// @exec: {"yield_time_ms": 0}\nawait tools.Pause({}); text(await tools.wait_agent({agent_ids:[load("child")],timeout_ms:10000})); try { await tools.SecretFailure({}); } catch (error) { text(error); }');
      case 2: {
        const cell = history.match(/Script running with cell ID ([^\\"\s]+)/)?.[1]; assert.ok(cell, history);
        return { type: 'tool_use', id: 'wait-cell', name: 'wait', input: { cell_id: cell } };
      }
      case 3:
        assert.match(history, /CHILD_OK/); assert.match(history, /PRIVATE_HOST_DETAIL/);
        return final('STRICT_CLAUDE_OK');
      default: throw new Error('unexpected request');
    }
  });
  let agent, failure;
  try {
    agent = await Claude.create({ ...f.options, subagents: { maxConcurrency: 2 }, tools: [
      { name: 'Read', description: 'Read synthetic content', handler() { effects.push('read'); return { content: 'READ_OK' }; } },
      { name: 'Pause', description: 'Hold a cell briefly', async handler() { await new Promise(resolve => setTimeout(resolve, 100)); return 'PAUSE_OK'; } },
      { name: 'SecretFailure', description: 'Synthetic private error', handler() { throw new Error('PRIVATE_HOST_DETAIL'); } },
    ] });
    assert.equal((await agent.turn.prompt({ input: 'STRICT_CLAUDE_ROOT' }).result()).finalMessage, 'STRICT_CLAUDE_OK');
    stale = true;
    assert.equal((await agent.turn.prompt({ input: 'Reject old direct tool call' }).result()).finalMessage, 'STALE_DIRECT_DENIED');
    assert.deepEqual(effects, ['read']); assert.deepEqual(f.errors, []);
  } catch (error) { failure = error; throw error; }
  finally {
    await agent?.session.shutdown().catch(() => {});
    await evidence('native-children-wait', { status: failure ? 'failed' : 'passed', error: failure?.stack, effects, ...f, options: undefined });
  }
});

test('Claude code-only turn completion cancels abandoned cells and preserves the next turn', { timeout: 20_000 }, async t => {
  let step = 0, blockedSignal;
  const started = Promise.withResolvers();
  const f = await fixture(t, () => {
    switch (step++) {
      case 0: return exec('abandon-cell', '// @exec: {"yield_time_ms": 0}\nawait tools.Hold({});');
      case 1: return final('FIRST_DONE');
      case 2: return exec('next-cell', 'text(await tools.Read({}));');
      default: return final('SECOND_DONE');
    }
  });
  const signals = [];
  let agent, failure;
  try {
    agent = await Claude.create({ ...f.options, tools: [
      { name: 'Hold', description: 'Hold until cancellation', handler(_input, context) {
        blockedSignal = context.signal; started.resolve();
        return new Promise(resolve => context.signal.addEventListener('abort', () => resolve('CANCELLED'), { once: true }));
      } },
      { name: 'Read', description: 'Read next turn', handler(_input, context) { signals.push(context.signal.aborted); return 'NEXT_OK'; } },
    ] });
    const first = agent.turn.prompt({ input: 'Leave a yielded cell running' });
    await started.promise;
    assert.equal((await first.result()).finalMessage, 'FIRST_DONE');
    assert.equal(blockedSignal.aborted, true);
    assert.equal((await agent.turn.prompt({ input: 'Independent next turn' }).result()).finalMessage, 'SECOND_DONE');
    assert.deepEqual(signals, [false]); assert.deepEqual(f.errors, []);
  } catch (error) { failure = error; throw error; }
  finally {
    await agent?.session.shutdown().catch(() => {});
    await evidence('turn-cleanup', { status: failure ? 'failed' : 'passed', error: failure?.stack, blockedAborted: blockedSignal?.aborted, signals, trace: f.trace, errors: f.errors });
  }
});

// Portable export fences the live host at a real pending effect boundary. No
// journal snapshots or native receipts are manufactured by these journeys.
import { createMemoryDurabilityStore, exportDurabilityState, importDurabilityState } from '../runtime/durability-store.mjs';

for (const mode of ['code-only']) {
  // Two owner losses while model requests are in flight, then one while a
  // replayed response's exec is running. A fresh response after a continuation
  // starts a new round whose exec has never run, so it executes exactly once.
  // An exec whose admitting response was settled before the loss may already
  // have dispatched effects, so recovery refuses it instead of rerunning it.
  test(`Claude active ${mode} request survives two durable reopenings and refuses a lost exec admission`, { timeout: 30_000 }, async t => {
    const trace = [], errors = [], effects = [];
    const firstEntered = Promise.withResolvers(), replacementEntered = Promise.withResolvers(), heldEntered = Promise.withResolvers();
    const heldGate = Promise.withResolvers();
    const PENDING = 'Continue the admitted operation', HELD = 'Read once while the owner is lost', AFTER = 'Perform a fresh read after reconciling the recovery.';
    let pendingRequests = 0;
    const server = createServer(async (request, response) => {
      try {
        const parts = []; for await (const part of request) parts.push(part);
        const body = JSON.parse(Buffer.concat(parts)); trace.push(body);
        assert.deepEqual(body.tools.map(tool => tool.name).sort(), ['exec', 'wait']);
        // Decide from the current operation's prompt; history keeps earlier ones.
        const prompt = body.messages.findLast(message => message.role === 'user'
          && [PENDING, HELD, AFTER].some(text => JSON.stringify(message.content).includes(text)));
        const current = JSON.stringify(prompt?.content ?? '');
        const since = JSON.stringify(body.messages.slice(body.messages.indexOf(prompt) + 1));
        let block;
        if (current.includes(PENDING)) {
          pendingRequests++;
          if (pendingRequests === 1) { firstEntered.resolve(); return; }
          // Crash again after the recovered request was admitted; its payload
          // must remain stable across the second reopen.
          if (pendingRequests === 2) { replacementEntered.resolve(); return; }
          block = since.includes('UPGRADE_READ_OK') ? final('UPGRADE_DONE') : exec(`upgrade-read-${pendingRequests}`, 'text(await tools.Read({}));');
        } else if (current.includes(HELD)) {
          block = !since.includes('held-read') ? exec('held-read', 'text(await tools.Read({hold:true}));')
            : final(/admission was lost during recovery/.test(since) ? 'HELD_UNKNOWN' : 'HELD_REDISPATCHED');
        } else if (current.includes(AFTER)) {
          block = since.includes('UPGRADE_READ_OK') ? final('AFTER_DONE') : exec('after-read', 'text(await tools.Read({}));');
        } else throw new Error('unexpected synthetic Claude request');
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        response.end(messages(block, block.type === 'text' ? 'end_turn' : 'tool_use'));
      } catch (error) { errors.push(error.stack); response.destroy(error); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(async () => { heldGate.resolve(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
    const stateId = `claude-upgrade-pending-${mode}`;
    let store = createMemoryDurabilityStore(stateId), agent, failure;
    const prompt = { input: PENDING, id: 'upgrade-operation' };
    const held = { input: HELD, id: 'held-operation' };
    const options = {
      codeEvaluator: createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant)),
      model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic' },
      endpoint: `http://127.0.0.1:${server.address().port}/v1/messages`,
      module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)),
      durabilityId: stateId,
      tools: [{ name: 'Read', description: 'Read synthetic content', async handler(input) {
        if (input?.hold) { effects.push('held-read'); heldEntered.resolve(); await heldGate.promise; }
        else effects.push('read');
        return { content: 'UPGRADE_READ_OK' };
      } }],
    };
    // The archive is taken while work is in flight; the old owner then stops
    // before anything it does can reach the restored store.
    const reopen = async (result, afterCancel = () => {}) => {
      const archive = await exportDurabilityState(store, stateId);
      await agent.session.cancel().catch(() => {}); afterCancel(); await result;
      await agent.session.shutdown().catch(() => {}); agent = undefined;
      store = createMemoryDurabilityStore(stateId); await importDurabilityState(store, archive);
      agent = await Claude.create({ ...options, durability: store, subagents: { maxConcurrency: 2 } });
    };
    const results = id => trace.at(-1).messages.flatMap(message => Array.isArray(message.content) ? message.content : [])
      .filter(block => block.type === 'tool_result' && block.tool_use_id === id);
    try {
      agent = await Claude.create({ ...options, durability: store });
      let result = agent.turn.prompt(prompt).result().catch(error => error);
      await firstEntered.promise; await reopen(result);
      result = agent.turn.prompt(prompt).result().catch(error => error);
      await replacementEntered.promise; await reopen(result);
      result = agent.turn.prompt(prompt).result();
      assert.equal((await result).finalMessage, 'UPGRADE_DONE');
      assert.deepEqual(trace[1], trace[2], 'request remains frozen across a second restart');
      assert.deepEqual(effects, ['read'], 'the fresh post-recovery exec runs exactly once');
      const fresh = results('upgrade-read-3');
      assert.equal(fresh.length, 1, 'the fresh exec reports one result');
      assert.match(JSON.stringify(fresh[0].content), /UPGRADE_READ_OK/);
      assert.doesNotMatch(JSON.stringify(trace.at(-1).messages), /admission was lost during recovery/, 'a fresh response is not refused');

      // The response admitting held-read is settled, then its owner is lost
      // while the read is still running.
      result = agent.turn.prompt(held).result().catch(error => error);
      await heldEntered.promise; await reopen(result, () => heldGate.resolve());
      assert.equal((await agent.turn.prompt(held).result()).finalMessage, 'HELD_UNKNOWN');
      assert.deepEqual(effects, ['read', 'held-read'], 'lost code admission is not redispatched');
      const lost = results('held-read');
      assert.equal(lost.length, 1);
      assert.equal(lost[0].is_error, true);
      assert.match(JSON.stringify(lost[0].content), /admission was lost during recovery[\s\S]*outcome unknown/);

      assert.equal((await agent.turn.prompt({ input: AFTER, id: 'after-recovery' }).result()).finalMessage, 'AFTER_DONE');
      assert.deepEqual(effects, ['read', 'held-read', 'read']); assert.deepEqual(errors, []);
      assert.deepEqual(trace[0].tools.map(tool => tool.name).sort(), ['exec', 'wait']);
    } catch (error) { failure = error; throw error; }
    finally {
      heldGate.resolve();
      await agent?.session.shutdown().catch(() => {});
      await evidence(`upgrade-pending-${mode}`, { status: failure ? 'failed' : 'passed', error: failure?.stack, effects, trace, errors });
    }
  });
}

test('Claude code-only resumes an interrupted automatic compaction without enabling tools', { timeout: 30_000 }, async t => {
  const trace = [], errors = [], effects = [];
  const held = Promise.withResolvers();
  const server = createServer(async (request, response) => {
    try {
      const parts = []; for await (const part of request) parts.push(part);
      const body = JSON.parse(Buffer.concat(parts)); trace.push(body);
      if (trace.length === 2) {
        assert.equal(body.tool_choice?.type, 'none');
        assert.deepEqual(body.tools.map(tool => tool.name).sort(), ['exec', 'wait']);
        held.resolve(); return;
      }
      let block;
      if (trace.length === 1) block = final('SEED_CONTEXT_RETAINED');
      else {
        assert.deepEqual(body.tools.map(tool => tool.name).sort(), ['exec', 'wait']);
        const history = JSON.stringify(body.messages);
        if (body.tool_choice?.type === 'none') {
          assert.match(history, /SEED_CONTEXT_RETAINED/);
          assert.doesNotMatch(history, /provider-side effects may already have occurred/);
          block = final('SUMMARY_CONTEXT_RETAINED');
        } else {
          assert.match(history, /SUMMARY_CONTEXT_RETAINED/);
          assert.match(history, /SECOND_USER_TASK/);
          block = final('COMPACTION_UPGRADE_DONE');
        }
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(messages(block, 'end_turn'));
    } catch (error) { errors.push(error.stack); response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  const stateId = 'claude-upgrade-compaction', store = createMemoryDurabilityStore(stateId);
  const options = {
    codeEvaluator: createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant)),
    model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic' },
    endpoint: `http://127.0.0.1:${server.address().port}/v1/messages`,
    module: await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)),
    durabilityId: stateId, autoCompactWindowTokens: 1,
    tools: [{ name: 'Read', description: 'Synthetic read', handler() { effects.push('read'); return 'READ'; } }],
  };
  let agent, failure;
  const prompt = { input: 'SECOND_USER_TASK', id: 'compaction-upgrade-operation' };
  try {
    agent = await Claude.create({ ...options, durability: store });
    assert.equal((await agent.turn.prompt({ input: 'Seed prior context', id: 'seed-operation' }).result()).finalMessage, 'SEED_CONTEXT_RETAINED');
    const pending = agent.turn.prompt(prompt).result().catch(error => error);
    await held.promise;
    const archive = await exportDurabilityState(store, stateId);
    await agent.session.cancel().catch(() => {}); await pending;
    await agent.session.shutdown().catch(() => {}); agent = undefined;
    const recovered = createMemoryDurabilityStore(stateId); await importDurabilityState(recovered, archive);
    agent = await Claude.create({ ...options, durability: recovered, toolMode: 'code-only', subagents: { maxConcurrency: 2 },
      codeEvaluator: createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant)),
    });
    assert.equal((await agent.turn.prompt(prompt).result()).finalMessage, 'COMPACTION_UPGRADE_DONE');
    assert.equal(trace[2].tool_choice?.type, 'none');
    assert.deepEqual(effects, []); assert.deepEqual(errors, []);
  } catch (error) { failure = error; throw error; }
  finally {
    await agent?.session.shutdown().catch(() => {});
    await evidence('upgrade-compaction', { status: failure ? 'failed' : 'passed', error: failure?.stack, effects, trace, errors });
  }
});
