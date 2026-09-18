import { env, runInDurableObject, evictDurableObject } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { projectFollowupTurnId } from '../src/project-threads';
import { ProjectThreadRuns, projectCompletionInput } from '../src/project-thread-runs';

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const auth = JSON.stringify({ capabilities: ['agents:read', 'agents:write', 'tools:use'] });
function blockModel(session: DurableAgentSession) {
  const runtime = (session as unknown as { env: Record<string, unknown> }).env;
  Object.defineProperty(session, 'env', { value: { ...runtime, NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
    throw Object.assign(new Error('test retains work at durable retry boundary'), { code: 'retryable' });
  } } } });
}
async function setup(id: string) {
  const stub = sessions().getByName(id);
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

  it('admits independent senders using the same follow-up id into an unrelated conversation', async () => {
    const targetId = crypto.randomUUID(), target = await setup(targetId);
    const senders = [crypto.randomUUID(), crypto.randomUUID()];
    for (const senderId of senders) {
      const sender = await setup(senderId);
      await runInDurableObject(sender, async (session, state) => {
        const runs = new ProjectThreadRuns(state.storage);
        const turnId = projectFollowupTurnId(senderId, 'review');
        runs.put({ id: 'review', agent_id: targetId, turn_id: turnId, title: 'Unrelated conversation',
          input: `Reference from ${senderId}`, request_hash: senderId, authorization_json: auth, authorization_epoch: 1 });
        await session.alarm();
        expect(runs.get('review')?.state).toBe('watching');
        runs.finish('review', 'retired');
        await state.storage.deleteAlarm();
      });
    }
    await runInDurableObject(target, async (_session, state) => {
      const rows = state.storage.sql.exec<{ id: string; input_json: string }>('SELECT id,input_json FROM managed_turns ORDER BY rowid').toArray();
      expect(rows.map(row => row.id)).toEqual(senders.map(id => projectFollowupTurnId(id, 'review')));
      expect(rows.map(row => JSON.parse(row.input_json))).toEqual(senders.map(id => `Reference from ${id}`));
      state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
      await state.storage.deleteAlarm();
    });
  });

  it.each(['owner_id', 'organization_id', 'team_id', 'authorization_epoch'])('rejects a follow-up across the %s boundary', async column => {
    const sender = await setup(crypto.randomUUID()), targetId = crypto.randomUUID();
    const target = await setup(targetId);
    await runInDurableObject(target, async (_session, state) => {
      state.storage.sql.exec(`UPDATE session_state SET ${column}=?`, column === 'authorization_epoch' ? 2 : crypto.randomUUID());
    });
    await runInDurableObject(sender, async (session, state) => {
      const runs = new ProjectThreadRuns(state.storage);
      runs.put({ id: 'denied', agent_id: targetId, turn_id: 'project-followup:denied', title: 'Inaccessible',
        input: 'Must not arrive', request_hash: 'hash', authorization_json: auth, authorization_epoch: 1 });
      await session.alarm();
      expect(runs.get('denied')?.state).toBe('retired');
      await state.storage.deleteAlarm();
    });
    await runInDurableObject(target, async (_session, state) => {
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
