import { env, evictDurableObject, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MainThreadCompletions } from '../src/main-thread-completions';

const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;
const auth = JSON.stringify({ capabilities: ['agents:read', 'agents:write'] });

describe('durable main thread completion ledger', () => {
  it('persists deduplicated ordered completion pages across eviction', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    let first = 0;
    await runInDurableObject(stub, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      expect(completions.latestSequence()).toBe(0);
      expect(completions.entries(0)).toEqual([]);
      first = completions.publish('internal:first');
      expect(first).toBeGreaterThan(0);
      expect(completions.publish('internal:first')).toBe(first);
      for (let i = 0; i < 35; i++) completions.publish(`internal:${i}`);
    });
    await evictDurableObject(stub);
    await runInDurableObject(stub, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      expect(completions.publish('internal:first')).toBe(first);
      const page = completions.entries(0);
      expect(page).toHaveLength(32);
      expect(page[0]).toEqual({ sequence: first, turn_id: 'internal:first' });
      const remaining = completions.entries(page[31]!.sequence);
      expect(remaining).toHaveLength(4);
      const all = [...page, ...remaining];
      expect(new Set(all.map(entry => entry.turn_id)).size).toBe(36);
      expect(all.map(entry => entry.sequence)).toEqual(all.map(entry => entry.sequence).sort((a, b) => a - b));
      expect(completions.latestSequence()).toBe(remaining[3]!.sequence);
      expect(completions.entries(completions.latestSequence())).toEqual([]);
    });
  });

  it('retains the initial subscription and delivers late completions after the original response', async () => {
    const main = sessions().getByName(crypto.randomUUID());
    const coordinatorId = crypto.randomUUID();
    const coordinator = sessions().getByName(coordinatorId);
    let original = 0;
    await runInDurableObject(coordinator, async (_session, state) => {
      original = new MainThreadCompletions(state.storage).publish('original-response');
    });
    await runInDurableObject(main, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      expect(completions.get(coordinatorId)).toBeUndefined();
      const initial = completions.watch(coordinatorId, original, auth, 1);
      expect(completions.get(coordinatorId)).toEqual(initial);
      completions.retry(coordinatorId, 100);
      expect(completions.watch(coordinatorId, 0, 'wider authority', 2)).toEqual({ ...initial, retry_at: 100 });
      expect(completions.nextAlarm()).toBe(100);
      expect(completions.due(99)).toEqual([]);
    });
    await evictDurableObject(main);
    await evictDurableObject(coordinator);
    const late = await runInDurableObject(coordinator, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      completions.publish('project-result:late-child');
      return completions.entries(original);
    });
    expect(late).toHaveLength(1);
    expect(late[0]!.turn_id).toBe('project-result:late-child');
    await runInDurableObject(main, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      expect(completions.due(100)[0]).toMatchObject({ cursor: original, authorization_json: auth, authorization_epoch: 1 });
      completions.advance(coordinatorId, late[0]!.sequence);
      completions.advance(coordinatorId, original);
      completions.retry(coordinatorId, 200);
    });
    await evictDurableObject(main);
    await runInDurableObject(main, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      expect(completions.watch(coordinatorId, 0, 'replacement', 3)).toMatchObject({ cursor: late[0]!.sequence, authorization_json: auth, authorization_epoch: 1, retry_at: 200 });
      expect(completions.due(199)).toEqual([]);
      expect(completions.due(200)).toHaveLength(1);
    });
    await runInDurableObject(coordinator, async (_session, state) => {
      expect(new MainThreadCompletions(state.storage).entries(late[0]!.sequence)).toEqual([]);
    });
  });

  it('persists revocation tombstones and excludes them from polling', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      completions.watch('revoked', 7, auth, 1);
      completions.retire('revoked');
    });
    await evictDurableObject(stub);
    await runInDurableObject(stub, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      completions.retry('revoked', 0);
      completions.advance('revoked', 100);
      expect(completions.watch('revoked', 0, auth, 2)).toMatchObject({ state: 'retired', cursor: 7, authorization_epoch: 1, authorization_json: '' });
      expect(completions.due(Number.MAX_SAFE_INTEGER)).toEqual([]);
      expect(completions.nextAlarm()).toBeUndefined();
      expect(completions.reauthorize('revoked', 20, auth, 1).state).toBe('retired');
      expect(completions.reauthorize('revoked', 20, auth, 2)).toMatchObject({
        state: 'watching', cursor: 20, authorization_epoch: 2, authorization_json: auth,
      });
      expect(completions.reauthorize('revoked', 0, 'replacement', 3)).toMatchObject({
        state: 'watching', cursor: 20, authorization_epoch: 2, authorization_json: auth,
      });
    });
  });

  it('bounds polling and preserves separate coordinator cursors', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (_session, state) => {
      const completions = new MainThreadCompletions(state.storage);
      for (let i = 0; i < 10; i++) {
        completions.watch(`coordinator-${i}`, i, auth, 1);
        completions.retry(`coordinator-${i}`, i);
      }
      completions.advance('coordinator-0', 99);
      const due = completions.due(100);
      expect(due).toHaveLength(8);
      expect(due[0]!.cursor).toBe(99);
      expect(due[1]!.cursor).toBe(1);
      completions.retire('coordinator-0');
      expect(completions.nextAlarm()).toBe(1);
    });
  });
});

describe('main thread completion feed RPC isolation', () => {
  it('requires the coordinator owner and team and a nonnegative integer cursor', async () => {
    const id = crypto.randomUUID();
    const stub = sessions().getByName(id);
    const owner = '11111111-1111-4111-8111-111111111111';
    const team = '33333333-3333-4333-8333-333333333333';
    await runInDurableObject(stub, async (session, state) => {
      state.storage.sql.exec(`INSERT INTO session_state
        (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
        VALUES (1,?,?,'22222222-2222-4222-8222-222222222222',?,1,'https://nanocodex.example','managed',?)`,
      id, owner, team, Date.now());
      const completions = new MainThreadCompletions(state.storage);
      const first = completions.publish('project-result:first');
      const latest = completions.publish('project-result:second');
      expect(session.mainThreadCompletionFeed(owner, team, first)).toEqual({
        latest, data: [{ sequence: latest, turn_id: 'project-result:second' }], busy: false,
      });
      expect(() => session.mainThreadCompletionFeed('another-owner', team, 0)).toThrow('completion scope mismatch');
      expect(() => session.mainThreadCompletionFeed(owner, 'another-team', 0)).toThrow('completion scope mismatch');
      for (const cursor of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
        expect(() => session.mainThreadCompletionFeed(owner, team, cursor)).toThrow('completion scope mismatch');
      }
    });
    await evictDurableObject(stub);
    const accepted = await stub.mainThreadCompletionFeed(owner, team, 0);
    expect(accepted.data.map(entry => entry.turn_id)).toEqual(['project-result:first', 'project-result:second']);
  });

  it('does not expose an uninitialized session ledger through the RPC', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (session, state) => {
      new MainThreadCompletions(state.storage).publish('project-result:orphan');
      expect(() => session.mainThreadCompletionFeed('owner', 'team', 0)).toThrow('completion scope mismatch');
    });
  });
});

describe('bounded completion watch lifecycle', () => {
  it('backs off to five minutes, becomes idle only when caught up, and resumes without reviving revocation', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (_session, state) => {
      const ledger = new MainThreadCompletions(state.storage);
      ledger.watch('child', 0, auth, 1);
      for (let i = 0; i < 8; i++) ledger.settle(ledger.get('child')!, true, 0, false);
      expect(ledger.get('child')!.poll_delay).toBe(300_000);
      ledger.settle(ledger.get('child')!, false, 1, false);
      expect(ledger.get('child')!.state).toBe('watching');
      ledger.advance('child', 1);
      ledger.settle(ledger.get('child')!, false, 1, true);
      expect(ledger.get('child')).toMatchObject({ state: 'idle', authorization_json: '', cursor: 1 });
      expect(ledger.nextAlarm()).toBeUndefined();
      ledger.activate('child', auth, 1);
      expect(ledger.get('child')).toMatchObject({ state: 'watching', cursor: 1, poll_delay: 30_000 });
      ledger.retire('child');
      ledger.activate('child', auth, 1);
      expect(ledger.get('child')!.state).toBe('retired');
      expect(ledger.nextAlarm()).toBeUndefined();
    });
  });

  it('fences a stale idle observation after a newer admission and bounds active watches', async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (_session, state) => {
      const ledger = new MainThreadCompletions(state.storage);
      const old = ledger.watch('child', 0, auth, 1);
      ledger.activate('child', auth, 1);
      ledger.settle(old, false, 0, false);
      ledger.retire('child', old.generation);
      expect(ledger.get('child')!.state).toBe('watching');
      for (let i = 1; i < 128; i++) ledger.watch(`child-${i}`, 0, auth, 1);
      expect(() => ledger.watch('overflow', 0, auth, 1)).toThrow('128');
      ledger.settle(ledger.get('child')!, false, 0, false);
      ledger.watch('overflow', 0, auth, 1);
      expect(() => ledger.activate('child', auth, 1)).toThrow('128');
    });
  });
});
