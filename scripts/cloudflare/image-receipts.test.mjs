import assert from 'node:assert/strict';
import test from 'node:test';
import { imageReceiptStore } from './image-receipts.mjs';

const account = 'a'.repeat(32), input = 'b'.repeat(64), ref = 'c'.repeat(40);
const receipt = { version: 1, image: 'phone', input, ref: `registry.cloudflare.com/${account}/nanocodex-ci-phone@sha256:${'d'.repeat(64)}` };
function fixture() {
  const entries = [], statuses = new Map(), calls = [];
  const request = async call => {
    calls.push(call);
    if (call.method === 'POST' && call.path.endsWith('/deployments')) {
      const entry = { ...call.body, id: entries.length + 1, sha: call.body.ref };
      entries.unshift(entry); return entry;
    }
    if (call.path.includes('/statuses')) {
      const id = Number(call.path.split('/').at(-2));
      if (call.method === 'POST') { const result = { id: calls.length, ...call.body }; statuses.set(id, [result]); return result; }
      return statuses.get(id) ?? [];
    }
    return entries;
  };
  return { store: imageReceiptStore({ repository: 'fixture/repo', ref, request }), entries, statuses, calls };
}

test('published immutable receipt survives cache eviction and later unrelated image publications', async () => {
  const f = fixture();
  assert.equal(await f.store.restore('phone', account, input), null);
  await f.store.save(receipt, account, input);
  const newer = { ...receipt, input: 'e'.repeat(64) };
  await f.store.save(newer, account, newer.input);
  assert.deepEqual(await f.store.restore('phone', account, input), receipt);
  assert.equal(await f.store.restore('phone', account, 'f'.repeat(64)), null);
  assert.equal(f.calls.filter(call => call.method === 'POST').length, 4);
});

test('unfinished publication, foreign accounts and unknown history cannot be reused', async () => {
  const f = fixture(); await f.store.save(receipt, account, input);
  f.statuses.set(f.entries[0].id, [{ state: 'in_progress', environment: 'nanocodex-image-phone' }]);
  assert.equal(await f.store.restore('phone', account, input), null);
  await assert.rejects(f.store.save(receipt, 'e'.repeat(32), input));
  const unavailable = imageReceiptStore({ repository: 'fixture/repo', ref, request: async () => { throw Error('unavailable'); } });
  assert.equal(await unavailable.restore('phone', account, input), null);
  await assert.rejects(unavailable.save(receipt, account, input));
});

test('optional image and validator extension preserves default phone/sandbox boundary', async () => {
  const custom = { ...receipt, image: 'account-relay', ref: receipt.ref.replace('phone', 'account-relay') };
  const defaultStore = fixture().store;
  await assert.rejects(defaultStore.save(custom, account, input));
  let validated = 0;
  const calls = [];
  const store = imageReceiptStore({ repository: 'fixture/repo', ref, allowedImages: ['account-relay'],
    validate: (value, image, targetAccount, targetInput) => {
      validated++; assert.deepEqual(value, custom); assert.equal(image, 'account-relay');
      assert.equal(targetAccount, account); assert.equal(targetInput, input);
    },
    request: async call => {
      calls.push(call);
      if (call.path.endsWith('/statuses')) return { state: 'success', environment: 'nanocodex-image-account-relay' };
      return { id: 1, environment: 'nanocodex-image-account-relay', sha: ref };
    },
  });
  await store.save(custom, account, input);
  assert.equal(validated, 1);
  assert.equal(calls[0].body.payload.schema, 1);
  assert.equal(calls[0].body.payload.receipt.image, 'account-relay');
});
