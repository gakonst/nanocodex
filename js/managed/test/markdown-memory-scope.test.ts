import { env, runInDurableObject } from 'cloudflare:test';
import { expect, it, vi } from 'vitest';
import type { MemoryScope, MemoryScopeEnv } from '../src/memory-scope';
import type { MarkdownMemoryFlushReceipt } from '../src/markdown-memory-flush';
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

it('reports disabled automation and rejects a matching internal flush when AI is unconfigured', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  const session = crypto.randomUUID();
  const headers = { ...where.headers, 'x-nanocodex-subject-id': `agent:${session}` };
  const response = await call(where, 'flush', {
    boundary_id: crypto.randomUUID(), session_id: session,
    messages: [{ id: 'firsthand-user-evidence', role: 'user', text: 'Prefer concise status updates.' }], truncated: false,
  }, headers);
  expect(response.status).toBe(503);
  expect(await response.json()).toEqual({ error: 'memory_automation_unavailable' });
  expect(await (await call(where, 'status', {})).json()).toMatchObject({
    automation: 'disabled', semantic: { enabled: false, pending: 0 },
    consolidation: { next_at: null, pending: [], receipts: [] },
    flush: { attempts: 0, pending: 0, receipts: [] },
  });
});

it.each(['user subject', 'wrong session', 'missing mutation', 'missing owner', 'wrong owner', 'missing subject', 'team scope'])(
  'rejects an internal flush with %s before invoking automation', async mode => {
    const where = target(crypto.randomUUID(), 'team', 'alice', mode === 'team scope' ? 'team' : 'personal');
    const session = crypto.randomUUID();
    const headers: Record<string, string> = { ...where.headers, 'x-nanocodex-subject-id': `agent:${session}` };
    if (mode === 'user subject') headers['x-nanocodex-subject-id'] = 'user:alice';
    if (mode === 'wrong session') headers['x-nanocodex-subject-id'] = `agent:${crypto.randomUUID()}`;
    if (mode === 'missing mutation') delete headers['x-nanocodex-memory-mutation'];
    if (mode === 'missing owner') delete headers['x-nanocodex-private-memory-owner'];
    if (mode === 'wrong owner') headers['x-nanocodex-private-memory-owner'] = 'bob';
    if (mode === 'missing subject') delete headers['x-nanocodex-subject-id'];
    const response = await call(where, 'flush', {
      boundary_id: crypto.randomUUID(), session_id: session,
      messages: [{ id: 'source', role: 'user', text: 'Use the amber fixture.' }], truncated: false,
    }, headers);
    expect(response.status).toBe(403);
    expect(await (await call(where, 'status', {})).json()).toMatchObject({ flush: { attempts: 0, pending: 0, receipts: [] } });
  },
);

it('requires matching subject, organization and personal owner assertions for status', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  expect((await call(where, 'status', {})).status).toBe(200);
  for (const headers of [
    { ...where.headers, 'x-nanocodex-subject-id': '' },
    { ...where.headers, 'x-nanocodex-private-memory-owner': 'bob' },
  ]) expect((await call(where, 'status', {}, headers)).status).toBe(403);
  expect((await call(where, 'status', {}, { ...where.headers, 'x-nanocodex-organization-id': crypto.randomUUID() })).status).toBe(404);
  const readOnlyHeaders: Record<string, string> = { ...where.headers };
  delete readOnlyHeaders['x-nanocodex-memory-mutation'];
  expect((await call(where, 'status', {}, readOnlyHeaders)).status).toBe(200);
});

it('queues manual daily writes durably, reports them by owner, and does not requeue an append replay', async () => {
  const org = crypto.randomUUID();
  const where = target(org, 'team', 'alice', 'personal');
  const path = 'memory/2026-09-22.md';
  const append = { operation: 'append', path, expected_revision: 0, operation_id: 'manual-daily-note', content: 'The amber fixture is ready.' };
  expect(await (await call(where, 'write', append)).json()).toMatchObject({ ok: true, path, revision: 1 });
  const status = await (await call(where, 'status', {})).json();
  expect(status).toMatchObject({
    automation: 'disabled', semantic: { enabled: false, pending: 1 },
    consolidation: { next_at: expect.any(Number), attempts: 0, pending: [{ path, revision: 1, next_line: 1 }], receipts: [] },
    flush: { attempts: 0, receipts: [] },
  });
  expect(await (await call(where, 'write', append)).json()).toMatchObject({ ok: true, path, revision: 1 });
  expect(await (await call(where, 'status', {})).json()).toEqual(status);
  expect(await (await call(target(org, 'another-team', 'alice', 'personal'), 'status', {})).json()).toEqual(status);
  for (const other of [target(org, 'team', 'bob', 'personal'), target(org, 'team', 'alice', 'team')]) {
    expect(await (await call(other, 'status', {})).json()).toMatchObject({
      semantic: { pending: 0 }, consolidation: { next_at: null, pending: [] }, flush: { receipts: [] },
    });
  }
  expect(await (await call(where, 'write', { operation: 'put', path, expected_revision: 0, content: 'stale update' })).json())
    .toMatchObject({ ok: false });
  expect(await (await call(where, 'status', {})).json()).toEqual(status);
  expect(await (await call(where, 'write', { operation: 'delete', path, expected_revision: 1 })).json()).toMatchObject({ ok: true, revision: 2 });
  expect(await (await call(where, 'status', {})).json()).toMatchObject({ consolidation: { next_at: null, pending: [] } });
});

it('rolls back the document, append receipt and search index when consolidation enqueue fails', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  await call(where, 'status', {});
  await runInDurableObject(where.stub, async (memory, state) => {
    state.storage.sql.exec(`CREATE TRIGGER fail_memory_consolidation BEFORE INSERT ON markdown_consolidation_events
      BEGIN SELECT RAISE(ABORT, 'synthetic consolidation enqueue failure'); END`);
    const write = () => memory.fetch(new Request('https://memory.internal/markdown-memory/write', {
      method: 'POST', headers: where.headers,
      body: JSON.stringify({ operation: 'append', path: 'memory/2026-09-22.md', expected_revision: 0,
        operation_id: 'atomic-daily-note', content: 'atomic-turquoise-canary' }),
    }));
    try {
      const failure = await write();
      expect(failure.status).toBe(500);
      expect(await failure.json()).toMatchObject({ error: 'memory_scope_failed' });
      for (const table of ['markdown_memory_documents', 'markdown_memory_operations', 'markdown_memory_chunks',
        'markdown_memory_fts', 'markdown_memory_ai_items', 'markdown_consolidation_events', 'markdown_consolidation_jobs']) {
        expect(state.storage.sql.exec(`SELECT COUNT(*) AS count FROM ${table}`).one()).toEqual({ count: 0 });
      }
    } finally {
      state.storage.sql.exec('DROP TRIGGER fail_memory_consolidation');
    }
    expect(await (await write()).json()).toMatchObject({ ok: true, revision: 1 });
  });
  expect(await (await call(where, 'get', { path: 'memory/2026-09-22.md' })).json()).toMatchObject({ revision: 1, content: 'atomic-turquoise-canary' });
  expect(await (await call(where, 'status', {})).json()).toMatchObject({
    semantic: { pending: 1 }, consolidation: { pending: [{ path: 'memory/2026-09-22.md', revision: 1 }] },
  });
});

it('rejects secret-bearing daily notes before creating consolidation, semantic or append work', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  const path = 'memory/2026-09-22.md';
  const response = await call(where, 'write', { operation: 'append', path, expected_revision: 0,
    operation_id: 'secret-blocked', content: '-----BEGIN PRIVATE KEY-----\nsynthetic fixture only\n-----END PRIVATE KEY-----' });
  expect(response.status).toBe(422);
  expect(await response.json()).toMatchObject({ error: 'memory_secret_rejected' });
  expect(await (await call(where, 'get', { path })).json()).toMatchObject({ revision: 0, deleted: true });
  expect(await (await call(where, 'status', {})).json()).toMatchObject({
    semantic: { pending: 0 }, consolidation: { next_at: null, pending: [] }, flush: { attempts: 0, pending: 0, receipts: [] },
  });
  await runInDurableObject(where.stub, async (_memory, state) => {
    expect(state.storage.sql.exec('SELECT COUNT(*) AS count FROM markdown_memory_operations').one()).toEqual({ count: 0 });
  });
});

it('persists an AI-backed flush through the internal RPC and exposes its queue, alarm and durable receipt', async () => {
  const where = target(crypto.randomUUID(), 'team', 'alice', 'personal');
  const session = crypto.randomUUID();
  const content = 'I prefer concise project status updates.';
  const input = { boundary_id: crypto.randomUUID(), session_id: session, truncated: false,
    messages: [{ id: 'firsthand-user-source', role: 'user', text: content }] };
  await runInDurableObject(where.stub, async (memory, state) => {
    // Replace only this fresh instance's environment before its first initialized request.
    const runtime = memory as unknown as { env: MemoryScopeEnv };
    const original = runtime.env;
    const run = vi.fn(async (_model: string, _input: Record<string, unknown>) => ({ response: JSON.stringify({
      spans: [{ message_id: input.messages[0]!.id, start: 0, end: content.length, quote: content }],
    }) }));
    runtime.env = { ...original, AI: { run } };
    const headers = { ...where.headers, 'x-nanocodex-subject-id': `agent:${session}` };
    const rpc = (operation: string, body: unknown) => memory.fetch(new Request(`https://memory.internal/markdown-memory/${operation}`, {
      method: 'POST', headers, body: JSON.stringify(body),
    }));
    try {
      expect(await (await rpc('status', {})).json()).toMatchObject({ automation: 'enabled', flush: { attempts: 0, receipts: [] } });
      const response = await rpc('flush', input);
      expect(response.status).toBe(200);
      const receipt = await response.json<MarkdownMemoryFlushReceipt>();
      expect(receipt).toMatchObject({ boundary_id: input.boundary_id, request_hash: expect.stringMatching(/^[a-f0-9]{64}$/),
        path: expect.stringMatching(/^memory\/\d{4}-\d{2}-\d{2}-flush-[a-f0-9]+\.md$/), revision: 1, selected_spans: 1, truncated: false });
      const document = await (await rpc('get', { path: receipt.path })).json<{ content: string; revision: number }>();
      expect(document.revision).toBe(1);
      expect(document.content).toContain(content);
      expect(document.content).toContain(`Session: ${session}`);
      const status = await (await rpc('status', {})).json<{ consolidation: { next_at: number } }>();
      expect(status).toMatchObject({
        automation: 'enabled', semantic: { enabled: false, pending: expect.any(Number) },
        consolidation: { next_at: expect.any(Number), pending: [{ path: receipt.path, revision: 1, next_line: 1 }] },
        flush: { attempts: 1, pending: 0, receipts: [receipt] },
      });
      expect(await state.storage.getAlarm()).toBe(status.consolidation.next_at);
      expect(run).toHaveBeenCalledTimes(1);
      expect(await (await rpc('flush', input)).json()).toEqual(receipt);
      expect(run).toHaveBeenCalledTimes(1);
      expect(await (await rpc('get', { path: receipt.path })).json()).toMatchObject({ revision: 1 });
      expect((await rpc('flush', { ...input, truncated: true })).status).toBe(409);
      expect(run).toHaveBeenCalledTimes(1);
      // Exercise the production alarm dispatch, including its rescheduling decision.
      run.mockResolvedValue({ response: JSON.stringify({ candidates: [] }) });
      state.storage.sql.exec('UPDATE markdown_consolidation_jobs SET due=? WHERE owner=?', Date.now() - 1, 'personal:alice');
      await memory.alarm();
      expect(run).toHaveBeenCalledTimes(2);
      expect(await (await rpc('status', {})).json()).toMatchObject({
        consolidation: { next_at: null, pending: [], receipts: [{ status: 'empty', sources: 1 }] },
        flush: { attempts: 1, receipts: [receipt] },
      });
      expect(await state.storage.getAlarm()).toBeNull();
    } finally {
      runtime.env = original;
      await state.storage.deleteAlarm();
    }
  });
});
