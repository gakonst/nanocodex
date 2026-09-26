import { env, runInDurableObject } from 'cloudflare:test';
import { expect, it, vi } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import type { MemoryScope, MemoryScopeEnv } from '../src/memory-scope';
import { memoryTarget } from '../src/memory-target';

const bindings = env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope>;
};
const source = { boundary_id: 'capture-fixture', session_id: '018f0000-0000-7000-8000-000000000001',
  messages: [{ id: 'user-one', role: 'user', text: 'My consulting business is inactive and has no customers.', created_at: 1 }] };
function memory() {
  const organization = crypto.randomUUID();
  const target = memoryTarget(organization, 'team', 'fixture-owner', 'personal');
  const stub = bindings.NANOCODEX_MEMORY.getByName(target.name);
  const headers = { 'x-nanocodex-organization-id': organization, 'x-nanocodex-team-id': target.team,
    'x-nanocodex-private-memory-owner': 'fixture-owner', 'x-nanocodex-memory-initialize': '1',
    'x-nanocodex-subject-id': `agent:${source.session_id}`, 'x-nanocodex-memory-mutation': '1' };
  const call = (operation: string, body: unknown, assertions = headers) => stub.fetch(`https://memory.internal/markdown-memory/${operation}`, {
    method: 'POST', headers: assertions, body: JSON.stringify(body),
  });
  return { stub, call, headers };
}

it('starts each decision on arrival without an alarm or a subsequent conversation turn', async () => {
  const m = memory();
  const calls: string[] = [];
  await runInDurableObject(m.stub, async (scope) => {
    const runtimeEnv = (scope as unknown as { env: MemoryScopeEnv }).env;
    const ai: MemoryScopeEnv['AI'] = { run: async (model, payload) => {
      calls.push(model);
      const questions = (payload as { questions: Record<string, unknown> }).questions;
      return { answers: Object.fromEntries(Object.keys(questions).map(key => [key, { choice: 'retain', confidence: 0.99 }])) };
    } };
    Object.defineProperty(scope, 'env', { value: { ...runtimeEnv, AI: ai, NANOCODEX_MEMORY_AUTOMATION: 'true' } });
  });
  expect((await m.call('capture', source)).status).toBe(202);
  await expect.poll(() => calls.length).toBe(1);
  await expect.poll(async () => {
    const status = await (await m.call('status', {})).json() as { capture: { receipts: unknown[] } };
    return status.capture.receipts.length;
  }).toBe(1);
  expect(calls).toEqual(['typesafe/jev']);
  expect(await (await m.call('status', {})).json()).toMatchObject({ capture: { mode: 'on_message', active: 0, attempts: 1 } });
  const notes = await (await m.call('search', { query: 'consulting' })).json();
  expect(JSON.stringify(notes)).toContain('My consulting business is inactive and has no customers.');
  expect(await (await m.call('capture', source)).json()).toMatchObject({ status: 'accepted' });
  expect(calls).toHaveLength(1);
});

it('requires private runtime attestation, mutation permission and enabled automation', async () => {
  const m = memory();
  expect((await m.call('capture', source, { ...m.headers, 'x-nanocodex-subject-id': 'user:fixture-owner' })).status).toBe(403);
  expect((await m.call('capture', source, { ...m.headers, 'x-nanocodex-memory-mutation': '0' })).status).toBe(403);
  expect((await m.call('capture', source, { ...m.headers, 'x-nanocodex-team-id': 'team' })).status).toBe(403);
  await runInDurableObject(m.stub, async scope => {
    const runtimeEnv = (scope as unknown as { env: MemoryScopeEnv }).env;
    Object.defineProperty(scope, 'env', { value: { ...runtimeEnv, NANOCODEX_MEMORY_AUTOMATION: 'false' } });
  });
  expect((await m.call('capture', source)).status).toBe(503);
  expect((await m.call('capture', { session_id: source.session_id, boundary_id: source.boundary_id, cancel: true })).status).toBe(202);
});

it.each(['api_key', 'service'])('admits real %s input and runs recovery independently of capture', async principalKind => {
  const stub = bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (session, state) => {
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    let release!: () => void;
    const deliveries: unknown[] = [];
    const memoryFetch = vi.fn(async (url: string, init?: RequestInit) => {
      if (new URL(url).pathname === '/markdown-memory/capture') {
        deliveries.push(JSON.parse(init!.body as string));
        return new Promise<Response>(resolve => { release = () => resolve(new Response(null, { status: 503 })); });
      }
      return new Response(null, { status: 204 });
    });
    Object.defineProperty(session, 'env', { value: {
      ...runtimeEnv, NANOCODEX_MEMORY_AUTOMATION: 'true',
      NANOCODEX: { fetch: async () => Response.json({ connectors: {}, mcp_connections: [], vault: [], tools: [], machines: [], connections: [], accounts: {} }) },
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: memoryFetch }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => { throw Object.assign(new Error('fixture runtime unavailable'), { code: 'retryable' }); } },
    } });
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID(), sessionId = crypto.randomUUID();
    state.storage.sql.exec(`INSERT INTO session_state (
      singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active
    ) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', ?)`, sessionId, owner, organization, team, Date.now());
    const headers = { 'Idempotency-Key': 'memory-unavailable', 'x-nanocodex-owner-id': owner,
      'x-nanocodex-session-organization-id': organization, 'x-nanocodex-session-team-id': team,
      'x-nanocodex-authorization-epoch': '1', 'x-nanocodex-request-principal': JSON.stringify({ kind: principalKind, user_id: owner }), 'x-nanocodex-capabilities': JSON.stringify(['agents:write', 'tools:use', 'memory:read', 'memory:write']) };
    try {
      const response = await session.fetch(new Request('https://session.internal/turns', { method: 'POST', headers,
        body: JSON.stringify({ id: 'memory-unavailable', input: 'I prefer short written updates.' }) }));
      expect(response.status).toBe(202);
      if (principalKind === 'service') {
        // Scheduled/callback service input retains full capabilities, but is not firsthand speech.
        expect(deliveries).toEqual([]);
      } else {
        await expect.poll(() => deliveries.length).toBe(1);
        expect(deliveries[0]).toMatchObject({ messages: [{ role: 'user', text: 'I prefer short written updates.' }] });
      }
      await expect(session.alarm()).resolves.toBeUndefined();
      expect(state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_turns WHERE id='memory-unavailable'").one().state).not.toBe('failed');
    } finally { release?.(); }
  });
});
