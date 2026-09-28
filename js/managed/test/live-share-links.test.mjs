// Production-only revoke journey. Never print bearer URLs or API credentials.
// Run after deployment in the Cloudflare live-validation workflow (suite: share).
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import test from 'node:test';

const origin = 'https://nanocodex.gakonst.workers.dev';
const key = process.env.NANOCODEX_SHARE_TEST_API_KEY;

// Failure modes: owner key forbidden, link scoped to the wrong thread, guest
// reads cached after revocation, revoked metadata listed, or failed cleanup.
test('production owner key creates and revokes a guest link', { timeout: 90_000 }, async t => {
  assert.ok(key, 'NANOCODEX_SHARE_TEST_API_KEY is required');
  const request = async (path, method = 'GET', body, bearer = key) => {
    const response = await fetch(`${origin}${path}`, { method, signal: AbortSignal.timeout(15_000),
      headers: { authorization: `Bearer ${bearer}`,
        ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    return response;
  };
  const created = await request('/v1/agents', 'POST', { settings: {
    model: 'gpt-6-luna', thinking: 'low', reasoning_mode: 'standard', fast_mode: false,
  } });
  assert.equal(created.status, 201, `create agent returned ${created.status}`);
  const { agent_id: id } = await created.json();
  assert.match(id, /^[0-9a-f-]{36}$/);
  t.after(async () => {
    const deleted = await request(`/v1/agents/${id}`, 'DELETE');
    assert.ok([200, 204, 404].includes(deleted.status), `cleanup returned ${deleted.status}`);
  });
  const owner = `/v1/agents/${id}`;
  const linkResponse = await request(`${owner}/share-links`, 'POST', { permission: 'read' });
  assert.equal(linkResponse.status, 201, `create share link returned ${linkResponse.status}`);
  const link = await linkResponse.json();
  assert.ok(URL.canParse(link.url), 'invalid share URL');
  const url = new URL(link.url);
  assert.equal(url.origin, origin);
  assert.equal(url.pathname, `/share/${id}`);
  const document = await fetch(`${origin}${url.pathname}`, { signal: AbortSignal.timeout(15_000) });
  assert.equal(document.status, 200, `guest website route returned ${document.status}`);
  assert.match(document.headers.get('content-type') ?? '', /text\/html/);
  assert.match(link.id, /^[0-9a-f-]{36}$/);
  assert.equal(link.permission, 'read');
  const token = url.hash.slice('#token='.length);
  assert.ok(/^nsl_[A-Za-z0-9_-]{43}$/.test(token), 'invalid bearer token shape');
  assert.equal((await request(`/v1/shared/${id}`, 'GET', undefined, token)).status, 200);
  assert.equal((await request(`/v1/shared/${randomUUID()}`, 'GET', undefined, token)).status, 404);
  const listed = await (await request(`${owner}/share-links`)).json();
  assert.equal(listed.data.some(item => item.id === link.id), true);
  assert.equal(JSON.stringify(listed).includes(token), false);
  const revoked = await request(`${owner}/share-links/${link.id}`, 'DELETE');
  assert.equal(revoked.status, 204);
  assert.equal((await request(`/v1/shared/${id}`, 'GET', undefined, token)).status, 404);
  assert.equal((await request(`/v1/shared/${id}/events/history`, 'GET', undefined, token)).status, 404);
  const remaining = await request(`${owner}/share-links`);
  assert.equal(remaining.status, 200);
  assert.equal((await remaining.json()).data.some(item => item.id === link.id), false);
  t.diagnostic(`revoke confirmed for synthetic managed thread ${id}; no bearer logged`);
});
