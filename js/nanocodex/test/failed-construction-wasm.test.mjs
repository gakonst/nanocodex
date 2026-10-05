// Public constructors share one generated WASM instance through every failure.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { Agent, Claude, Transport } from '../host/index.mjs';
import { initializeBrowserEngine } from '../browser/engine.mjs';
import { createMemoryDurabilityStore } from '../runtime/durability-store.mjs';

const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
for (const family of ['openai', 'claude']) {
  test(`${family} failed child-journal acquisition releases unpublished WASM capabilities`, async t => {
    const store = createMemoryDurabilityStore(`failed-create-${family}`);
    let fail = true, acquisitions = 0;
    const durability = { ...store, acquire(id, request) {
      if (id.endsWith('/children')) {
        acquisitions++;
        if (fail) throw new Error('synthetic child journal acquire failure');
      }
      return store.acquire(id, request);
    } };
    const options = { module, subagents: {}, durability, durabilityId: '018f1f9a-7b3c-7a07-8000-000000000099',
      sessionId: '018f1f9a-7b3c-7a07-8000-000000000099' };
    const create = family === 'openai'
      ? () => Agent.create({ ...options, transport: Transport.openAi({ apiKey: 'synthetic-current-auth' }) })
      : () => Claude.create({ ...options, model: 'claude-sonnet-4-6', auth: { apiKey: 'synthetic-current-auth' } });
    for (let index = 0; index < 8; index++) await assert.rejects(create(), /synthetic child journal acquire failure/);
    const engine = await initializeBrowserEngine({ module });
    const warm = engine.memory.buffer.byteLength;
    for (let index = 8; index < 48; index++) await assert.rejects(create(), /synthetic child journal acquire failure/);
    const retained = engine.memory.buffer.byteLength;
    assert.equal(acquisitions, 48);
    assert.equal(retained, warm, 'failed parent capabilities must not accumulate in the shared WASM heap');
    fail = false;
    const agent = await create();
    assert.equal(agent.sessionId, options.sessionId);
    await agent.session.shutdown();
    agent.dispose();
    t.diagnostic(JSON.stringify({ family, failedCreates: 48, warmBytes: warm, retainedBytes: retained }));
  });
}
