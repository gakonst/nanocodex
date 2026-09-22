import { env } from 'cloudflare:test';
import { expect, it } from 'vitest';
import type { MemoryScope } from '../src/memory-scope';
import { memoryTarget } from '../src/memory-target';

const binding = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
function target(org: string, team: string, user: string, scope: 'team' | 'personal') {
  const address = memoryTarget(org, team, user, scope);
  return { stub: binding.getByName(address.name), headers: {
    'x-nanocodex-organization-id': org, 'x-nanocodex-team-id': address.team,
    'x-nanocodex-memory-initialize': '1', 'x-nanocodex-subject-id': `user:${user}`,
    'x-nanocodex-memory-mutation': '1',
    ...(scope === 'personal' ? { 'x-nanocodex-private-memory-owner': user } : {}),
  } };
}
function call(where: ReturnType<typeof target>, operation: string, body: unknown, headers: HeadersInit = where.headers) {
  return where.stub.fetch(`https://memory.internal/markdown-memory/${operation}`, {
    method: 'POST', headers, body: JSON.stringify(body),
  });
}
it('keeps private Markdown across teams while isolating users, organizations and team memory', async () => {
  const org = crypto.randomUUID();
  const personal = target(org, 'team-a', 'alice', 'personal');
  expect((await call(personal, 'write', { operation: 'put', path: 'MEMORY.md', expected_revision: 0, content: 'private-copper-canary' })).status).toBe(200);
  expect(await (await call(target(org, 'team-b', 'alice', 'personal'), 'get', { path: 'MEMORY.md' })).json()).toMatchObject({ content: 'private-copper-canary' });
  for (const other of [target(org, 'team-a', 'bob', 'personal'), target(org, 'team-a', 'alice', 'team'), target(crypto.randomUUID(), 'team-a', 'alice', 'personal')]) {
    expect(await (await call(other, 'search', { query: 'private-copper-canary' })).json()).toMatchObject({ results: [] });
    expect(await (await call(other, 'get', { path: 'MEMORY.md' })).json()).toMatchObject({ deleted: true });
    expect(await (await call(other, 'bootstrap', {})).text()).not.toContain('private-copper-canary');
  }
});
it('requires live mutation authority and matching private owner before reading any document', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  const headers: Record<string,string> = { ...where.headers };
  delete headers['x-nanocodex-memory-mutation'];
  expect((await call(where, 'write', { operation: 'put', path: 'MEMORY.md', expected_revision: 0, content: 'blocked' }, headers)).status).toBe(403);
  headers['x-nanocodex-private-memory-owner'] = 'bob';
  expect((await call(where, 'get', { path: 'MEMORY.md' }, headers)).status).toBe(403);
  delete headers['x-nanocodex-private-memory-owner'];
  expect((await call(where, 'search', { query: 'blocked' }, headers)).status).toBe(403);
  expect((await call(where, 'get', { path: '../MEMORY.md' })).status).toBe(400);
  expect((await call(where, 'unsupported', {})).status).toBe(404);
});
it('removes replaced and deleted facts from both search and fresh bootstrap', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'team');
  await call(where, 'write', { operation: 'put', path: 'USER.md', expected_revision: 0, content: 'obsolete-lantern' });
  await call(where, 'write', { operation: 'put', path: 'USER.md', expected_revision: 1, content: 'current-silver' });
  expect(await (await call(where, 'search', { query: 'obsolete-lantern' })).json()).toMatchObject({ results: [] });
  expect(await (await call(where, 'bootstrap', {})).text()).toContain('current-silver');
  await call(where, 'write', { operation: 'delete', path: 'USER.md', expected_revision: 2 });
  expect(await (await call(where, 'search', { query: 'current-silver' })).json()).toMatchObject({ results: [] });
  expect(await (await call(where, 'bootstrap', {})).text()).not.toContain('current-silver');
});

it('projects canonical documents through the existing file API without changing ad-hoc note semantics', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  await call(where, 'write', { operation: 'put', path: 'MEMORY.md', expected_revision: 0, content: 'canonical-jade' });
  const legacy = (operation: string, body: unknown) => where.stub.fetch(`https://memory.internal/extension-memories/${operation}`, {
    method: 'POST', headers: where.headers, body: JSON.stringify(body),
  });
  expect(await (await legacy('files', {})).json()).toContain('MEMORY.md');
  expect(await (await legacy('file', { path: 'MEMORY.md' })).json()).toBe('canonical-jade');
  await call(where, 'write', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 });
  expect(await (await legacy('files', {})).json()).not.toContain('MEMORY.md');
});
it('applies the existing secret screen before persisting or indexing Markdown', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  const rejected = await call(where, 'write', { operation: 'put', path: 'MEMORY.md', expected_revision: 0,
    content: '-----BEGIN PRIVATE KEY-----\nsynthetic fixture, not a credential\n-----END PRIVATE KEY-----' });
  expect(rejected.status).toBe(422);
  expect(await (await call(where, 'get', { path: 'MEMORY.md' })).json()).toMatchObject({ revision: 0, deleted: true });
  expect(await (await call(where, 'search', { query: 'synthetic fixture' })).json()).toMatchObject({ results: [] });
});
