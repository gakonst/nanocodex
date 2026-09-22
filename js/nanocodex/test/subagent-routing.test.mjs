import assert from 'node:assert/strict';
import test from 'node:test';
import { createSubagentRouting } from '../runtime/subagent-routing.mjs';

function fixture(routes = new Map(), resolve = async ({ task }) => task === 'simple'
  ? { provider: 'vercel', model: 'astra', thinking: 'low', secret: 'must-not-persist' }
  : { provider: 'openrouter', model: 'sol', thinking: 'high' }) {
  let calls = 0;
  const router = createSubagentRouting({
    resolve: async (...args) => { calls++; return resolve(...args); },
    authorize: async (parent, context) => {
      if (context !== 'owned' || !['large-parent', 'small-parent'].includes(parent)) throw new Error('unauthorized');
      return { allowedProviders: ['vercel', 'openrouter'] };
    },
    load: id => routes.get(id), save: (id, route) => routes.set(id, route),
  });
  return { router, routes, calls: () => calls };
}

test('large parent downgrades and small parent escalates across providers; routes stay pinned in live memory', async () => {
  const { router, routes, calls } = fixture();
  for (const [parentSessionId, task, sessionId, provider, model] of [
    ['large-parent', 'simple', 'small-child', 'vercel', 'astra'],
    ['small-parent', 'hard', 'large-child', 'openrouter', 'sol'],
  ]) {
    const request = { parentSessionId, task, role: 'worker', hostContextRef: 'owned' };
    const choice = await router.resolve(request);
    router.bind({ ...request, sessionId, routeId: choice.routeId });
    assert.equal(router.route(sessionId).provider, provider);
    assert.equal(router.route(sessionId).model, model);
    assert.equal(JSON.stringify(router.route(sessionId)).includes('secret'), false);
  }
  assert.equal(calls(), 2);
  const reopened = fixture(routes, () => { throw new Error('must not reroute'); });
  assert.deepEqual(reopened.router.route('small-child'), router.route('small-child'));
  assert.deepEqual(reopened.router.route('large-child'), router.route('large-child'));
  assert.equal(reopened.calls(), 0);
  const restarted = fixture();
  assert.throws(() => restarted.router.route('small-child'), /missing/);
  assert.throws(() => restarted.router.route('large-child'), /missing/);
});

test('authorization fails before routing; route tickets cannot cross parent authority', async () => {
  const { router, calls } = fixture();
  await assert.rejects(router.resolve({ parentSessionId: 'large-parent', hostContextRef: 'wrong' }), /unauthorized/);
  assert.equal(calls(), 0);
  const request = { parentSessionId: 'large-parent', hostContextRef: 'owned', task: 'simple' };
  const choice = await router.resolve(request);
  assert.throws(() => router.bind({ ...request, parentSessionId: 'small-parent', sessionId: 'child', routeId: choice.routeId }), /not owned/);
  router.bind({ ...request, sessionId: 'child', routeId: choice.routeId });
  assert.throws(() => router.bind({ ...request, sessionId: 'other', routeId: choice.routeId }), /not owned/);
  assert.throws(() => router.route('unknown'), /refusing to inherit/);
});

test('explicit child choices must be honored or rejected; never silently substituted', async () => {
  const { router } = fixture();
  const request = { parentSessionId: 'large-parent', hostContextRef: 'owned', task: 'simple' };
  await assert.rejects(router.resolve({ ...request, model: 'sol' }), /explicit model/);
  await assert.rejects(router.resolve({ ...request, thinking: 'high' }), /explicit thinking/);
  assert.equal((await router.resolve({ ...request, model: 'astra', thinking: 'low' })).model, 'astra');
});

test('equivalent public model aliases preserve explicit overrides and pin canonical models', async () => {
  for (const [model, alias] of [['sol', 'gpt-5.6-sol'], ['terra', 'gpt-5.6-terra'],
    ['luna', 'gpt-5.6-luna'], ['astra', 'gpt-6-astra'], ['glm-5.3', '@cf/zai-org/glm-5.3'], ['glm-5.3', 'glm53']]) {
    for (const [requested, selected] of [[model, alias], [alias, model]]) {
      const { router } = fixture(new Map(), () => ({ provider: 'test', model: selected, thinking: 'low' }));
      const request = { parentSessionId: 'large-parent', hostContextRef: 'owned', model: requested };
      const choice = await router.resolve(request);
      assert.equal(choice.model, model);
      router.bind({ ...request, routeId: choice.routeId, sessionId: 'child' });
      assert.equal(router.route('child').model, model);
    }
  }
});

test('authorization and explicit overrides use the request captured before awaiting', async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  let resolved;
  const router = createSubagentRouting({
    authorize: async (parent, context) => { assert.equal(parent, 'parent'); assert.equal(context, 'owned'); await gate; },
    resolve: request => { resolved = request; return { provider: 'test', model: 'sol', thinking: 'high' }; },
    load: () => undefined, save: () => {},
  });
  const request = { parentSessionId: 'parent', hostContextRef: 'owned', model: 'sol', thinking: 'high', task: 'original' };
  const pending = router.resolve(request);
  Object.assign(request, { parentSessionId: 'other', hostContextRef: 'other', model: 'astra', thinking: 'low', task: 'changed' });
  release();
  const choice = await pending;
  assert.deepEqual(resolved, { parentSessionId: 'parent', hostContextRef: 'owned', model: 'sol', thinking: 'high', task: 'original' });
  assert.ok(Object.isFrozen(resolved));
  assert.throws(() => router.bind({ ...request, sessionId: 'child', routeId: choice.routeId }), /not owned/);
  router.bind({ parentSessionId: 'parent', hostContextRef: 'owned', sessionId: 'child', routeId: choice.routeId });
});

test('failed live route saves block binding and keep the ticket available for retry', async () => {
  const routes = new Map();
  let fail = true;
  const router = createSubagentRouting({
    authorize: () => {}, resolve: () => ({ provider: 'test', model: 'sol', thinking: 'high' }),
    load: id => routes.get(id), save: (id, route) => { if (fail) throw new Error('storage failed'); routes.set(id, route); },
  });
  const { routeId } = await router.resolve({ parentSessionId: 'parent' });
  const binding = { parentSessionId: 'parent', sessionId: 'child', routeId };
  assert.throws(() => router.bind(binding), /storage failed/);
  assert.throws(() => router.route('child'), /missing/);
  fail = false;
  router.bind(binding);
  const second = await router.resolve({ parentSessionId: 'parent' });
  assert.throws(() => router.bind({ ...binding, routeId: second.routeId }), /already pinned/);
});

test('asynchronous route load and save implementations fail closed', async () => {
  for (const operation of ['load', 'save']) {
    const router = createSubagentRouting({
      authorize: () => {}, resolve: () => ({ provider: 'test', model: 'sol', thinking: 'high' }),
      load: () => undefined, save: () => {}, [operation]: async () => { throw new Error('async failure'); },
    });
    const { routeId } = await router.resolve({ parentSessionId: 'parent' });
    assert.throws(() => router.bind({ parentSessionId: 'parent', sessionId: 'child', routeId }), new RegExp(`${operation} must be synchronous`));
    if (operation === 'load') assert.throws(() => router.route('child'), /load must be synchronous/);
  }
});

test('invalid models and public route fields cannot be bound', async () => {
  for (const fields of [{ model: 'unknown' }, { thinking: 'unknown' }, { provider: ' ' }, { providerModel: {} }]) {
    const { router } = fixture(new Map(), () => ({ provider: 'test', model: 'sol', thinking: 'high', ...fields }));
    await assert.rejects(router.resolve({ parentSessionId: 'large-parent', hostContextRef: 'owned' }), /invalid/);
  }
});

test('the WASM bridge refuses asynchronous custom route bindings', async () => {
  const { installHostBridge, registerDefinitionHost, releaseDefinitionHost } = await import('../internal.mjs');
  installHostBridge();
  const id = registerDefinitionHost({ bindSubagentRoute: async () => { throw new Error('not synchronous'); } });
  try {
    assert.throws(() => globalThis.nanocodexHost.bindSubagentRoute(id, '{}'), /binding must be synchronous/);
  } finally {
    releaseDefinitionHost(id);
  }
});
