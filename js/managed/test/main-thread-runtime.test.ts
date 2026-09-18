import { env, runInDurableObject } from 'cloudflare:test';
import { expect, it, vi } from 'vitest';
import { MainThreadCompletions } from '../src/main-thread-completions';
import type { Env } from '../src/index';

// SQL seeds identity/configuration and a stale revoked watch only. The scripted
// provider drives production tool calls and terminal transitions for every turn.
it('executes Main routing, a persistent child, and both internal outcome turns through the production runtime', async () => {
  const base = env as unknown as Env;
  const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
  const mainId = crypto.randomUUID().replace(/^(.{14})./, '$17');
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${owner}\0canonical:${team}:project:runtime`))).slice(0, 16);
  digest[6] = (digest[6]! & 15) | 128; digest[8] = (digest[8]! & 63) | 128;
  const hex = [...digest].map(byte => byte.toString(16).padStart(2, '0')).join('');
  const expectedCoordinatorId = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  const settings = { model: 'gpt-5.6-terra', thinking: 'high', reasoning_mode: 'standard', fast_mode: false };
  const capabilities = ['agents:read', 'agents:write', 'tools:use'];
  const configuration = { instructions: 'Inherited runtime fixture instructions',
    multi_agent: { enabled: true, max_concurrent_subagents: 2 },
    environment: { files: [], skills: [], setup_commands: [], network: { access: 'enabled' } } };
  const installed = new Set<string>();
  const calls = new Map<string, unknown[]>();
  let coordinatorId: string | undefined;
  let childId: string | undefined;
  let offset = 0;
  const realNow = Date.now.bind(Date);
  const clock = vi.spyOn(Date, 'now').mockImplementation(() => realNow() + offset);
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor(readonly id: string) { super(); }
    accept() {}
    close() { this.readyState = 3; }
    send(data: string) {
      const request = JSON.parse(data);
      const history = calls.get(this.id) ?? [];
      history.push(request); calls.set(this.id, history);
      const first = history.length === 1;
      const name = this.id === mainId ? 'route_project' : this.id === coordinatorId ? 'spawn_project_thread' : undefined;
      const args = name === 'route_project'
        ? { project_id: 'runtime', name: 'Runtime project', id: 'route', input: 'Delegate the fixture task to a persistent child.' }
        : { id: 'child', title: 'Runtime child', input: 'Complete the fixture task and report evidence.' };
      const output = first && name ? [{ type: 'function_call', call_id: `call-${this.id}`, name, arguments: JSON.stringify(args) }]
        : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text:
          this.id === mainId ? 'Main processed the outcome.' : this.id === coordinatorId ? 'Coordinator processed the outcome.' : 'Child completed with actual runtime evidence.' }] }];
      queueMicrotask(() => this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify({
        type: 'response.completed', response: { id: `response-${this.id}-${history.length}`, status: 'completed',
          end_turn: !(first && name), output, usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 } },
      }) })));
    }
  }
  const users = { getByName: (id: string) => ({ fetch: (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init);
    if (new URL(request.url).pathname === '/account') return Promise.resolve(Response.json({ id, organizationId: organization, persistent: true, createdAt: 1, lastAuthenticatedAt: 1 }));
    return base.NANOCODEX_USERS.getByName(id).fetch(request);
  } }) };
  async function install(id: string) {
    if (installed.has(id)) return;
    installed.add(id);
    if (id !== mainId) { if (!coordinatorId) coordinatorId = id; else if (id !== coordinatorId) childId = id; }
    await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(id), async (session) => {
      Object.defineProperty(session, 'env', { configurable: true, value: { ...base,
        NANOCODEX_USERS: users,
        MANAGED_BROWSER_PROVIDER: 'cloudflare', LOADER: {}, BROWSER: {},
        NANOCODEX_ORGANIZATIONS: { getByName: () => ({ fetch: async () => Response.json({ organizationId: organization,
          teamId: team, role: 'owner', authorizationEpoch: 2, capabilities }) }) },
        NANOCODEX_SESSIONS: { idFromName: (name: string) => base.NANOCODEX_SESSIONS.idFromName(name), getByName: (target: string) => new Proxy({}, { get: (_object, key) => async (...args: unknown[]) => {
          await install(target);
          const stub = base.NANOCODEX_SESSIONS.getByName(target);
          return (stub as unknown as Record<string, (...values: unknown[]) => unknown>)[String(key)]!(...args);
        } }) },
        NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
          const request = new Request(input, init);
          if (request.url === 'https://nanocodex.internal/v1/responses') return { status: 101, headers: new Headers(), webSocket: new ModelSocket(id) };
          if (request.url.startsWith('https://broker.internal/subjects/')) return new Response(null, { status: 204 });
          if (request.url.endsWith('/catalog')) return Response.json({ connectors: {}, mcp_connections: [] });
          return Response.json({ data: [] });
        } },
      } });
    });
  }
  const rows = (id: string) => runInDurableObject(base.NANOCODEX_SESSIONS.getByName(id), async (_session, state) =>
    state.storage.sql.exec<{ id: string; state: string; input_json: string; terminal_json: string | null; error: string | null }>(
      'SELECT id,state,input_json,terminal_json,error FROM managed_turns').toArray());
  try {
    await install(mainId);
    await runInDurableObject(base.NANOCODEX_USERS.getByName(owner), async (_account, state) => {
      await state.storage.put('account', { id: owner, organizationId: organization, persistent: true, createdAt: 1, lastAuthenticatedAt: 1 });
      state.storage.sql.exec('INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,?,?,?)', mainId, Date.now(), Date.now(), team);
      state.storage.sql.exec('INSERT INTO main_threads(team_id,agent_id) VALUES (?,?)', team, mainId);
    });
    await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(mainId), async (session, state) => {
      state.storage.sql.exec(`INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
        VALUES (1,?,?,?,?,2,'https://nanocodex.example','managed',?)`, mainId, owner, organization, team, Date.now());
      state.storage.sql.exec('INSERT INTO managed_configuration(singleton,body) VALUES (1,?)', JSON.stringify(configuration));
      state.storage.sql.exec('UPDATE managed_agent_settings SET model=?,thinking=?,reasoning_mode=?,fast_mode=0 WHERE singleton=1', settings.model, settings.thinking, settings.reasoning_mode);
      const ledger = new MainThreadCompletions(state.storage);
      ledger.watch(expectedCoordinatorId, 0, JSON.stringify({ capabilities }), 1);
      ledger.retire(expectedCoordinatorId);
      expect(ledger.get(expectedCoordinatorId)?.state).toBe('retired');
      const response = await session.fetch(new Request('https://session.internal/turns', { method: 'POST', headers: {
        'x-nanocodex-owner-id': owner, 'x-nanocodex-session-organization-id': organization,
        'x-nanocodex-session-team-id': team, 'x-nanocodex-authorization-epoch': '2',
        'x-nanocodex-capabilities': JSON.stringify(capabilities),
      }, body: JSON.stringify({ id: 'user-task', input: 'Route this task to the runtime project and delegate it.' }) }));
      expect(response.status).toBe(202);
    });
    for (let attempt = 0; attempt < 80; attempt++) {
      offset += 31_000;
      for (const id of installed) await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(id), async (session) => { await session.alarm(); });
      const main = await rows(mainId);
      if (main.some(row => row.state === 'failed')) throw new Error(JSON.stringify(main));
      if (main.some(row => row.id.startsWith('main-result:') && row.input_json.includes('project-result:') && row.state === 'completed')) break;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    expect(coordinatorId).toBe(expectedCoordinatorId); expect(childId).toBeDefined();
    await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(mainId), async (_session, state) => {
      expect(new MainThreadCompletions(state.storage).get(expectedCoordinatorId)).toMatchObject({ authorization_epoch: 2 });
    });
    const child = await rows(childId!);
    expect(child).toHaveLength(1);
    expect(child[0]).toMatchObject({ state: 'completed', error: null });
    expect(child[0]!.terminal_json).toContain('Child completed with actual runtime evidence.');
    const coordinator = await rows(coordinatorId!);
    expect(coordinator.every(row => row.state === 'completed')).toBe(true);
    expect(coordinator.find(row => row.id.startsWith('project-result:'))).toMatchObject({ state: 'completed', error: null });
    expect(coordinator.find(row => row.id.startsWith('project-result:'))!.terminal_json).toContain('Coordinator processed the outcome.');
    const main = await rows(mainId);
    expect(main.every(row => row.state === 'completed')).toBe(true);
    const outcome = main.find(row => row.id.startsWith('main-result:') && row.input_json.includes('project-result:'));
    expect(outcome).toMatchObject({ state: 'completed', error: null });
    expect(outcome!.terminal_json).toContain('Main processed the outcome.');
    for (const id of [coordinatorId!, childId!]) await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(id), async (_session, state) => {
      expect(JSON.parse(state.storage.sql.exec<{ body: string }>('SELECT body FROM managed_configuration').one().body)).toMatchObject(configuration);
      expect(state.storage.sql.exec('SELECT model,thinking,reasoning_mode,fast_mode FROM managed_agent_settings').one()).toEqual({ ...settings, fast_mode: 0 });
    });
    for (const requests of calls.values()) for (const request of requests) expect((request as { model: string }).model).toBe(settings.model);
    expect(calls.get(mainId)!.length).toBeGreaterThanOrEqual(3);
    expect(calls.get(coordinatorId!)!.length).toBeGreaterThanOrEqual(3);
    expect(calls.get(childId!)).toHaveLength(1);
  } finally {
    for (const id of installed) await runInDurableObject(base.NANOCODEX_SESSIONS.getByName(id), async (_session, state) => {
      // Fixture cleanup only, after all runtime assertions; no fabricated outcomes.
      state.storage.sql.exec("UPDATE main_thread_completion_watches SET state='retired',authorization_json=''");
      state.storage.sql.exec("UPDATE project_thread_runs SET state='retired'");
      state.storage.sql.exec('DELETE FROM history_projection_outbox');
      await state.storage.deleteAlarm();
    });
    clock.mockRestore();
  }
}, 60_000);
