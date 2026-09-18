import { env, runInDurableObject, evictDurableObject } from 'cloudflare:test';
import { afterEach, describe, expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { ProjectThreadRuns, projectCompletionInput } from '../src/project-thread-runs';

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const fixtures: ReturnType<ReturnType<typeof sessions>['getByName']>[] = [];
afterEach(async () => {
  for (const stub of fixtures.splice(0)) await runInDurableObject(stub, async (_session, state) => {
    // Model dispatch is intentionally blocked in these tests. Release its retained
    // alarms and synthetic history receipts after assertions, not during the scenario.
    state.storage.sql.exec("UPDATE main_thread_completion_watches SET state='retired',authorization_json=''");
    state.storage.sql.exec("UPDATE project_thread_runs SET state='retired'");
    state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL WHERE state IN ('accepted','cancelling')");
    state.storage.sql.exec('DELETE FROM history_projection_outbox');
    await state.storage.deleteAlarm();
  });
});
const auth = JSON.stringify({ capabilities: ['agents:read', 'agents:write', 'tools:use'] });
function blockModel(session: DurableAgentSession, epoch = 1) {
  const runtime = (session as unknown as { env: Record<string, unknown> }).env;
  const users = runtime.NANOCODEX_USERS as DurableObjectNamespace;
  Object.defineProperty(session, 'env', { value: { ...runtime,
    NANOCODEX_USERS: { getByName: (id: string) => ({ fetch: (input: Request | string, init?: RequestInit) =>
      new URL(typeof input === 'string' ? input : input.url).pathname === '/account'
        ? Promise.resolve(Response.json({ id, organizationId: '22222222-2222-4222-8222-222222222222', persistent: true, createdAt: 1, lastAuthenticatedAt: 1 }))
        : users.getByName(id).fetch(input, init) }) },
    NANOCODEX_ORGANIZATIONS: { getByName: () => ({ fetch: async () => Response.json({
      organizationId: '22222222-2222-4222-8222-222222222222', teamId: '33333333-3333-4333-8333-333333333333',
      role: 'writer', authorizationEpoch: epoch, capabilities: JSON.parse(auth).capabilities }) }) },
    NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
    throw Object.assign(new Error('test retains work at durable retry boundary'), { code: 'retryable' });
  } } } });
}
async function setup(id: string) {
  const owner = '11111111-1111-4111-8111-111111111111';
  const registry = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS.getByName(owner);
  await runInDurableObject(registry, async (_, state) => {
    // Project discovery validates the persisted account against live session identity.
    await state.storage.put('account', { id: owner, organizationId: '22222222-2222-4222-8222-222222222222', persistent: true, createdAt: 1, lastAuthenticatedAt: 1 });
  });
  const stub = sessions().getByName(id);
  fixtures.push(stub);
  await runInDurableObject(stub, async (session, state) => {
    blockModel(session);
    state.storage.sql.exec(`INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,'11111111-1111-4111-8111-111111111111','22222222-2222-4222-8222-222222222222','33333333-3333-4333-8333-333333333333',1,'https://nanocodex.example','managed',?)`, id, Date.now());
  });
  return stub;
}
async function terminal(stub: ReturnType<typeof sessions> extends DurableObjectNamespace<infer T> ? DurableObjectStub<T> : never, id: string, stateValue: string) {
  await runInDurableObject(stub, async (_session, state) => {
    state.storage.sql.exec('UPDATE managed_turns SET state=?,retry_at=NULL,terminal_json=? WHERE id=?', stateValue,
      JSON.stringify({ type: `turn_${stateValue}`, id, output: 'Verified task outcome' }), id);
    await state.storage.deleteAlarm();
  });
}

describe('persistent project outcome delivery in the managed runtime', () => {
  it.each(['completed', 'failed', 'cancelled'])('admits a child, survives parent eviction, and returns %s once without a user poll', async outcome => {
    const parentId = crypto.randomUUID(), childId = crypto.randomUUID();
    const parent = await setup(parentId), child = await setup(childId);
    await runInDurableObject(parent, async (session, state) => {
      const runs = new ProjectThreadRuns(state.storage);
      runs.put({ id: 'stable', agent_id: childId, turn_id: 'project:fix', title: 'Fix sign-in', input: 'Fix sign-in', request_hash: 'initial', authorization_json: auth, authorization_epoch: 1 });
      await session.alarm();
      expect(runs.get('stable')?.state).toBe('watching');
      expect(await state.storage.getAlarm()).not.toBeNull();
      // Lose the child admission acknowledgment, then replay the same durable intent.
      state.storage.sql.exec("UPDATE project_thread_runs SET state='admitting',retry_at=0");
      await session.alarm();
      expect(runs.get('stable')?.state).toBe('watching');
      state.storage.sql.exec('UPDATE project_thread_runs SET retry_at=0');
      await session.alarm();
      expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(0);
    });
    await runInDurableObject(child, async (_session, state) => {
      expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(1);
    });
    const result = await child.fetch(new Request('https://session.internal/turns/project:fix'));
    expect(result.status).toBe(200);
    expect(await result.json()).toMatchObject({ turn_id: 'project:fix', input: 'Fix sign-in' });
    await terminal(child, 'project:fix', outcome);
    await evictDurableObject(parent);
    await runInDurableObject(parent, async (session, state) => {
      blockModel(session);
      state.storage.sql.exec('UPDATE project_thread_runs SET retry_at=0');
      await session.alarm();
      const runs = new ProjectThreadRuns(state.storage);
      expect(runs.get('stable')?.state).toBe('delivered');
      const row = state.storage.sql.exec<{ id: string; input_json: string; authorization_json: string }>('SELECT id,input_json,authorization_json FROM managed_turns').one();
      expect(row.id).toBe('project-result:stable');
      expect(JSON.parse(row.input_json)).toContain('Internal project task completion');
      expect(JSON.parse(row.input_json)).toContain(`"state":"${outcome}"`);
      expect(JSON.parse(row.authorization_json)).toEqual(JSON.parse(auth));
      // Simulate loss of the completion acknowledgment after durable admission.
      state.storage.sql.exec("UPDATE project_thread_runs SET state='watching',retry_at=0");
      await session.alarm();
      expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(1);
      expect(runs.get('stable')?.state).toBe('delivered');
      state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
      await state.storage.deleteAlarm();
    });
  });

  it('does not widen authority or wake the parent after revocation', async () => {
    const parent = await setup(crypto.randomUUID());
    await runInDurableObject(parent, async (session, state) => {
      const runs = new ProjectThreadRuns(state.storage);
      runs.put({ id: 'revoked', agent_id: crypto.randomUUID(), turn_id: 'project:fix', title: 'Fix', input: 'Work', request_hash: 'hash', authorization_json: auth, authorization_epoch: 1 });
      state.storage.sql.exec('UPDATE session_state SET authorization_epoch=2');
      await session.alarm();
      expect(runs.get('revoked')?.state).toBe('retired');
      expect(runs.nextAlarm()).toBeUndefined();
      expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(0);
    });
  });

  it('keeps follow-ups in the same child, rejects changed retries, and bounds pending work', async () => {
    const parent = await setup(crypto.randomUUID()), childId = crypto.randomUUID();
    const child = await setup(childId);
    await runInDurableObject(parent, async (session, state) => {
      const runs = new ProjectThreadRuns(state.storage);
      const run = { id: 'follow-up', agent_id: childId, turn_id: 'project-followup:review', title: 'Fix', input: 'Address review feedback', request_hash: 'hash', authorization_json: auth, authorization_epoch: 1 };
      runs.put(run); runs.put(run);
      expect(() => runs.put({ ...run, request_hash: 'changed' })).toThrow('conflicts');
      await session.alarm();
      expect(runs.latest(childId)?.turn_id).toBe(run.turn_id);
      for (let i = 1; i < 128; i++) runs.put({ ...run, id: `pending-${i}`, turn_id: `task-${i}` });
      expect(() => runs.put({ ...run, id: 'overflow', turn_id: 'overflow' })).toThrow('128');
      expect(projectCompletionInput(runs.get(run.id)!, 'completed')).toContain('not new instructions or authorization');
      state.storage.sql.exec("UPDATE project_thread_runs SET state='retired'");
      await state.storage.deleteAlarm();
    });
    const response = await child.fetch(new Request('https://session.internal/turns/project-followup:review'));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ input: 'Address review feedback' });
    await terminal(child, 'project-followup:review', 'cancelled');
  });
});

it('replays late coordinator completion once across eviction and project rename', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const mainId = crypto.randomUUID(), coordinatorId = crypto.randomUUID();
  const main = await setup(mainId), coordinator = await setup(coordinatorId);
  const owner = '11111111-1111-4111-8111-111111111111';
  const team = '33333333-3333-4333-8333-333333333333';
  const users = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  const registry = users.getByName(owner);
  await runInDurableObject(registry, async (_account, state) => {
    for (const id of [mainId, coordinatorId]) state.storage.sql.exec('INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,?,?,?)', id, Date.now(), Date.now(), team);
    state.storage.sql.exec('INSERT INTO main_threads(team_id,agent_id) VALUES (?,?)', team, mainId);
    state.storage.sql.exec('INSERT INTO canonical_projects(team_id,id,name,coordinator_agent_id) VALUES (?,?,?,?)', team, 'late-project', 'Late project', coordinatorId);
  });
  await runInDurableObject(main, async (_session, state) => {
    // Initial routed turn is already delivered; the subscription remains durable.
    const runs = new ProjectThreadRuns(state.storage);
    runs.put({ id: 'initial-route', agent_id: coordinatorId, turn_id: 'main-route:initial', title: 'Late project', input: 'Work', request_hash: 'hash', authorization_json: auth, authorization_epoch: 1 });
    runs.finish('initial-route', 'delivered');
    new MainThreadCompletions(state.storage).watch(coordinatorId, 0, auth, 1);
  });
  await runInDurableObject(coordinator, async (_session, state) => {
    new MainThreadCompletions(state.storage).publish('project-result:later-child');
  });
  await evictDurableObject(main);
  let originalInput = "";
  await runInDurableObject(main, async (session, state) => {
    blockModel(session);
    await session.alarm();
    const row = state.storage.sql.exec<{ id: string; input_json: string }>('SELECT id,input_json FROM managed_turns').one();
    expect(row.id).toBe(`main-result:${coordinatorId}:1`);
    expect(JSON.parse(row.input_json)).toContain('"turn_id":"project-result:later-child"');
    expect(JSON.parse(row.input_json)).toContain('"project_id":"late-project"');
    expect(JSON.parse(row.input_json)).not.toContain('Late project');
    originalInput = row.input_json;
  });
  await runInDurableObject(registry, async (_account, registryState) => {
      registryState.storage.sql.exec('UPDATE canonical_projects SET name=? WHERE coordinator_agent_id=?', 'Renamed project', coordinatorId);
  });
  await runInDurableObject(main, async (session, state) => {
    // Lost admission acknowledgement followed by rename must replay the same body.
    state.storage.sql.exec("UPDATE main_thread_completion_watches SET cursor=0,retry_at=0,state='watching',authorization_json=?", auth);
    await session.alarm();
    expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(1);
    expect(new MainThreadCompletions(state.storage).get(coordinatorId)?.cursor).toBe(1);
    expect(state.storage.sql.exec<{ input_json: string }>('SELECT input_json FROM managed_turns').one().input_json).toBe(originalInput);
    state.storage.sql.exec('UPDATE session_state SET authorization_epoch=2');
    state.storage.sql.exec('UPDATE main_thread_completion_watches SET retry_at=0');
    await session.alarm();
    expect(new MainThreadCompletions(state.storage).nextAlarm()).toBeUndefined();
    state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
    await state.storage.deleteAlarm();
  });
  await runInDurableObject(registry, async (_account, state) => {
    state.storage.sql.exec('DELETE FROM canonical_projects WHERE coordinator_agent_id=?', coordinatorId);
    state.storage.sql.exec('DELETE FROM main_threads WHERE agent_id=?', mainId);
  });
});

it('commits late-result publication atomically with terminal state and rejects changed routing intent', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const { retainMainRoute } = await import('../src/main-thread');
  const { commitManagedTransition } = await import('../src/index');
  const { DurableEventLog } = await import('../src/durable-events');
  const stub = await setup(crypto.randomUUID());
  await runInDurableObject(stub, async (session, state) => {
    const input = { project_id: 'build', name: 'Build', id: 'stable', input: 'Implement it' };
    retainMainRoute(state.storage, input);
    retainMainRoute(state.storage, input);
    expect(() => retainMainRoute(state.storage, { ...input, project_id: 'different' })).toThrow('conflicts');
    expect(() => retainMainRoute(state.storage, { ...input, input: 'Different work' })).toThrow('conflicts');
    const id = 'project-result:atomic';
    expect((await session.fetch(new Request('https://session.internal/turns', {
      method: 'POST', body: JSON.stringify({ id, input: 'Report child outcome' }),
    }))).status).toBe(202);
    const ledger = new MainThreadCompletions(state.storage);
    const log = new DurableEventLog<Extract<import('../src/protocol').ServerMessage, { type: 'turn_completed' }>>(state.storage);
    const terminal = { type: 'turn_completed' as const, id, final_message: 'Done', usage: null, citations: [] };
    expect(() => commitManagedTransition(state.storage, log, id, terminal, () => {
      ledger.publish(id);
      throw new Error('injected failure');
    })).toThrow('injected failure');
    expect(ledger.entries(0)).toEqual([]);
    expect(state.storage.sql.exec<{ state: string }>('SELECT state FROM managed_turns WHERE id=?', id).one().state).toBe('accepted');
    commitManagedTransition(state.storage, log, id, terminal, () => ledger.publish(id));
    commitManagedTransition(state.storage, log, id, terminal, () => { throw new Error('duplicate publication'); });
    expect(ledger.entries(0)).toEqual([{ sequence: 1, turn_id: id }]);
    state.storage.sql.exec('DELETE FROM history_projection_outbox WHERE turn_id=?', id);
    await state.storage.deleteAlarm();
  });
});

it('propagates a late nested thread result to its direct coordinator without expanding project access', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const parentId = crypto.randomUUID(), childId = crypto.randomUUID();
  const parent = await setup(parentId), child = await setup(childId);
  const registry = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS.getByName('11111111-1111-4111-8111-111111111111');
  await runInDurableObject(registry, async (_account, state) => {
    for (const id of [parentId, childId]) state.storage.sql.exec('INSERT INTO agent_registry(id,created_at,updated_at) VALUES (?,1,1)', id);
    state.storage.sql.exec(`INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
      VALUES (?,?,?,'origin','project:child','Child','hash',1)`, childId, parentId, parentId);
  });
  await runInDurableObject(child, async (_session, state) => {
    new MainThreadCompletions(state.storage).publish('project-result:grandchild');
  });
  let originalInput = "";
  await runInDurableObject(parent, async (session, state) => {
    const ledger = new MainThreadCompletions(state.storage);
    ledger.watch(childId, 0, auth, 1);
    await session.alarm();
    const turn = state.storage.sql.exec<{ id: string; input_json: string }>('SELECT id,input_json FROM managed_turns').one();
    expect(turn.id).toBe(`project-result:late:${childId}:1`);
    expect(JSON.parse(turn.input_json)).toContain('read_project_thread');
    expect(JSON.parse(turn.input_json)).toContain('"turn_id":"project-result:grandchild"');
    expect(ledger.get(childId)?.cursor).toBe(1);
    originalInput = turn.input_json;
  });
  await runInDurableObject(registry, async (_account, registryState) => {
      registryState.storage.sql.exec('UPDATE project_threads SET title=? WHERE agent_id=?', 'Renamed child', childId);
  });
  await runInDurableObject(parent, async (session, state) => {
    const ledger = new MainThreadCompletions(state.storage);
    state.storage.sql.exec("UPDATE main_thread_completion_watches SET cursor=0,retry_at=0,state='watching',authorization_json=?", auth);
    await session.alarm();
    expect(ledger.get(childId)?.cursor).toBe(1);
    expect(state.storage.sql.exec('SELECT id FROM managed_turns').toArray()).toHaveLength(1);
    expect(state.storage.sql.exec<{ input_json: string }>('SELECT input_json FROM managed_turns').one().input_json).toBe(originalInput);
    ledger.retire(childId);
    state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
    await state.storage.deleteAlarm();
  });
});

it('retains late-result subscriptions through transient registry failures', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const parent = await setup(crypto.randomUUID());
  await runInDurableObject(parent, async (session, state) => {
    const runtime = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, 'env', { value: { ...runtime, NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 503 }) }) } } });
    const ledger = new MainThreadCompletions(state.storage);
    const child = crypto.randomUUID();
    ledger.watch(child, 7, auth, 1);
    await session.alarm();
    expect(ledger.get(child)).toMatchObject({ cursor: 7, state: 'watching', authorization_epoch: 1 });
    expect(ledger.nextAlarm()).toBeGreaterThan(Date.now());
    expect(state.storage.sql.exec<{ count: number }>('SELECT COUNT(*) AS count FROM managed_turns').one().count).toBe(0);
    ledger.retire(child);
    await state.storage.deleteAlarm();
  });
});

it('renews a revoked completion subscription while recovering a new authorized admission', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const parent = await setup(crypto.randomUUID()), childId = crypto.randomUUID();
  const child = await setup(childId);
  await runInDurableObject(child, async (_session, state) => {
    state.storage.sql.exec('UPDATE session_state SET authorization_epoch=2');
    new MainThreadCompletions(state.storage).publish('project-result:before-renewal');
  });
  await runInDurableObject(parent, async (session, state) => {
    const ledger = new MainThreadCompletions(state.storage);
    ledger.watch(childId, 0, auth, 1);
    state.storage.sql.exec('UPDATE session_state SET authorization_epoch=2');
    blockModel(session, 2);
    // Isolate loss occurred after saving this explicit new-epoch intent but before renewing its watch.
    const runs = new ProjectThreadRuns(state.storage);
    runs.put({ id: 'new-epoch', agent_id: childId, turn_id: 'project:renewed', title: 'Renewed', input: 'New authorized work', request_hash: 'new', authorization_json: auth, authorization_epoch: 2 });
    await session.alarm();
    expect(runs.get('new-epoch')?.state).toBe('watching');
    expect(ledger.get(childId)).toMatchObject({ state: 'watching', authorization_epoch: 2, cursor: 1 });
    runs.finish('new-epoch', 'retired');
    ledger.retire(childId);
    await state.storage.deleteAlarm();
  });
  await terminal(child, 'project:renewed', 'cancelled');
});

it('relays one late nested result child → coordinator → Main and retires idle watches without blocking export forever', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const { commitManagedTransition } = await import('../src/index');
  const { DurableEventLog } = await import('../src/durable-events');
  const mainId = crypto.randomUUID(), coordinatorId = crypto.randomUUID(), childId = crypto.randomUUID();
  const main = await setup(mainId), coordinator = await setup(coordinatorId), child = await setup(childId);
  const owner = '11111111-1111-4111-8111-111111111111', team = '33333333-3333-4333-8333-333333333333';
  const registry = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS.getByName(owner);
  await runInDurableObject(registry, async (_account, state) => {
    for (const id of [mainId, coordinatorId, childId]) state.storage.sql.exec('INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,1,1,?)', id, team);
    state.storage.sql.exec('INSERT INTO main_threads(team_id,agent_id) VALUES (?,?)', team, mainId);
    state.storage.sql.exec("INSERT INTO canonical_projects(team_id,id,name,coordinator_agent_id) VALUES (?,'nested','Nested',?)", team, coordinatorId);
    state.storage.sql.exec(`INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
      VALUES (?,?,?,'origin','project:child','Child','hash',1)`, childId, coordinatorId, coordinatorId);
  });
  for (const [parent, target] of [[main, coordinatorId], [coordinator, childId]] as const) {
    await runInDurableObject(parent, async (_session, state) => {
      // Initial responses have already finished. Only the recursive completion watch remains.
      new MainThreadCompletions(state.storage).watch(target, 0, auth, 1);
    });
  }
  const complete = async (stub: typeof child, id: string) => runInDurableObject(stub, async (_session, state) => {
    const log = new DurableEventLog<Extract<import('../src/protocol').ServerMessage, { type: 'turn_completed' }>>(state.storage);
    commitManagedTransition(state.storage, log, id, { type: 'turn_completed', id, final_message: 'Verified result', usage: null, citations: [] },
      () => new MainThreadCompletions(state.storage).publish(id));
    // This synthetic provider receipt exercises completion delivery, not memory
    // indexing. The fixture has no corresponding history projection grant.
    state.storage.sql.exec('DELETE FROM history_projection_outbox WHERE turn_id=?', id);
    await state.storage.deleteAlarm();
  });
  await runInDurableObject(child, async (session) => {
    expect((await session.fetch(new Request('https://session.internal/turns', { method: 'POST', body: JSON.stringify({ id: 'project-result:deep-task', input: 'Review deeper task result' }) }))).status).toBe(202);
  });
  // A still-running descendant prevents ancestors from declaring the subtree idle.
  await runInDurableObject(main, async (session, state) => {
    await session.alarm();
    expect(new MainThreadCompletions(state.storage).get(coordinatorId)?.state).toBe('watching');
  });
  await complete(child, 'project-result:deep-task');
  await evictDurableObject(coordinator);
  const coordinatorTurn = `project-result:late:${childId}:1`;
  await runInDurableObject(coordinator, async (session, state) => {
    blockModel(session);
    state.storage.sql.exec('UPDATE main_thread_completion_watches SET retry_at=0');
    await session.alarm();
    expect(state.storage.sql.exec<{ input_json: string }>('SELECT input_json FROM managed_turns WHERE id=?', coordinatorTurn).one().input_json).toContain('project-result:deep-task');
    expect(new MainThreadCompletions(state.storage).get(childId)?.state).toBe('idle');
    expect(session.mainThreadCompletionFeed(owner, team, 0).busy).toBe(true);
  });
  await complete(coordinator, coordinatorTurn);
  await evictDurableObject(main);
  const mainTurn = `main-result:${coordinatorId}:1`;
  await runInDurableObject(main, async (session, state) => {
    blockModel(session);
    state.storage.sql.exec('UPDATE main_thread_completion_watches SET retry_at=0');
    await session.alarm();
    const row = state.storage.sql.exec<{ input_json: string }>('SELECT input_json FROM managed_turns WHERE id=?', mainTurn).one();
    expect(row.input_json).toContain(coordinatorTurn);
    expect(row.input_json).toContain('read_project');
    const ledger = new MainThreadCompletions(state.storage);
    expect(ledger.get(coordinatorId)?.state).toBe('idle');
    expect(ledger.nextAlarm()).toBeUndefined();
    // The export subscription guard uses nextAlarm(); idle watches release it.
    // Finish the fixture's model-blocked report without starting durability I/O.
    state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
    expect(session.mainThreadCompletionFeed(owner, team, 0).busy).toBe(false);
    await state.storage.deleteAlarm();
  });
});

 it.each(['epoch', 'membership', 'tools', 'retained-tools'])('retires completion authority on %s revocation with cached session epoch unchanged', async reason => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const parent = await setup(crypto.randomUUID()), childId = crypto.randomUUID();
  await runInDurableObject(parent, async (session, state) => {
    const ledger = new MainThreadCompletions(state.storage);
    const retained = reason === 'retained-tools' ? JSON.stringify({ capabilities: ['agents:read', 'agents:write'] }) : auth;
    ledger.watch(childId, 0, retained, 1);
    const runs = new ProjectThreadRuns(state.storage);
    runs.put({ id: 'revoked-account', agent_id: childId, turn_id: 'task', title: 'Task', input: 'Work', request_hash: 'hash', authorization_json: retained, authorization_epoch: 1 });
    const runtime = (session as unknown as { env: Record<string, unknown> }).env;
    let feeds = 0;
    Object.defineProperty(session, 'env', { value: { ...runtime,
      NANOCODEX_SESSIONS: { getByName: () => ({ mainThreadCompletionFeed: () => { feeds++; throw new Error('unauthorized feed read'); } }) },
      NANOCODEX_ORGANIZATIONS: { getByName: () => ({ fetch: async () => reason === 'membership'
        ? new Response(null, { status: 403 }) : Response.json({
          organizationId: '22222222-2222-4222-8222-222222222222', teamId: '33333333-3333-4333-8333-333333333333', role: 'writer',
          authorizationEpoch: reason === 'epoch' ? 2 : 1,
          capabilities: reason === 'tools' ? ['agents:read', 'agents:write'] : JSON.parse(auth).capabilities }) }) },
    } });
    await session.alarm();
    expect(state.storage.sql.exec<{ authorization_epoch: number }>('SELECT authorization_epoch FROM session_state').one().authorization_epoch).toBe(1);
    expect(ledger.get(childId)?.state).toBe('retired');
    expect(runs.get('revoked-account')?.state).toBe('retired');
    expect(feeds).toBe(0);
    expect(state.storage.sql.exec('SELECT id FROM managed_turns').toArray()).toEqual([]);
  });
});

it('rechecks live authority after reading the feed and before internal admission', async () => {
  const { MainThreadCompletions } = await import('../src/main-thread-completions');
  const parentId = crypto.randomUUID(), childId = crypto.randomUUID();
  const parent = await setup(parentId);
  await runInDurableObject(parent, async (session, state) => {
    const runtime = (session as unknown as { env: Record<string, unknown> }).env;
    const users = runtime.NANOCODEX_USERS as DurableObjectNamespace;
    const organizations = runtime.NANOCODEX_ORGANIZATIONS as DurableObjectNamespace;
    let revoked = false, feeds = 0;
    Object.defineProperty(session, 'env', { value: { ...runtime,
      NANOCODEX_USERS: { getByName: (id: string) => ({ fetch: (input: string) => {
        const path = new URL(input).pathname;
        if (path === '/main-thread') return Promise.resolve(Response.json({ agent_id: parentId }));
        if (path === '/projects') return Promise.resolve(Response.json({ data: [{ id: 'project', name: 'Project', coordinator_agent_id: childId }] }));
        return users.getByName(id).fetch(input);
      } }) },
      NANOCODEX_ORGANIZATIONS: { getByName: (id: string) => ({ fetch: (input: string) => revoked
        ? Promise.resolve(new Response(null, { status: 403 })) : organizations.getByName(id).fetch(input) }) },
      NANOCODEX_SESSIONS: { getByName: () => ({ mainThreadCompletionFeed: async () => {
        feeds++; revoked = true;
        return { latest: 1, data: [{ sequence: 1, turn_id: 'project-result:late' }], busy: false };
      } }) },
    } });
    const ledger = new MainThreadCompletions(state.storage);
    ledger.watch(childId, 0, auth, 1);
    await session.alarm();
    expect(feeds).toBe(1);
    expect(ledger.get(childId)).toMatchObject({ state: 'retired', cursor: 0 });
    expect(state.storage.sql.exec('SELECT id FROM managed_turns').toArray()).toEqual([]);
  });
});
