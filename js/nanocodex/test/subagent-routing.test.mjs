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

test('large parent downgrades and small parent escalates across providers; routes survive reconstruction', async () => {
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
