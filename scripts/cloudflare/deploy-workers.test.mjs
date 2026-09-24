import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { deployPhase, phases } from './deploy-workers.mjs';

test('independent Workers start concurrently and each retains the release guard', async () => {
  const calls = [];
  const children = [];
  const pending = deployPhase('infrastructure', {
    env: { DEPLOY_MESSAGE: 'synthetic release', GITHUB_OUTPUT: 'must-not-share' },
    launch: (command, args, options) => {
      calls.push({ command, args, options });
      const child = new EventEmitter(); children.push(child); return child;
    },
  });
  assert.equal(calls.length, 3, 'all independent Workers must start before waiting');
  for (const call of calls) {
    assert(call.args[0].endsWith('/scripts/cloudflare/current-production-release.mjs'));
    assert.equal(call.args[1], '--');
    assert.equal(call.options.env.GITHUB_OUTPUT, undefined);
    assert.equal(call.args.at(-1), 'synthetic release');
  }
  children.forEach(child => child.emit('close', 0));
  await pending;
});

test('a failed consumer fails the phase; account is never in a parallel phase', async () => {
  assert.equal(phases.consumers.length, 6);
  assert(!Object.values(phases).flat().some(([name])=>name==='account'));
  await assert.rejects(deployPhase('consumers', {
    env: { DEPLOY_MESSAGE: 'synthetic release' },
    launch: () => { const child = new EventEmitter(); queueMicrotask(()=>child.emit('close',1)); return child; },
  }), /Worker deployments failed/);
  await assert.rejects(deployPhase('account'), /Unknown deployment phase/);
});
