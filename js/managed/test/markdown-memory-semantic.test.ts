import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MARKDOWN_MEMORY_CLEANUP_HORIZON_MS, MarkdownMemoryStore } from '../src/markdown-memory';
import { MarkdownMemorySemantic } from '../src/markdown-memory-semantic';

const put = (path: string, content: string, expected_revision = 0) => ({ operation: 'put', path, content, expected_revision });
type Remote = { id: string; key: string; status: string; metadata: Record<string, string>; content: string };
class FakeAi {
  records = new Map<string, Remote>();
  uploads = 0;
  searches: unknown[] = [];
  afterUpload?: () => void;
  uploadGate?: Promise<void>;
  uploadStarted?: () => void;
  failDelete = false;
  deletes = 0;
  beforeSearch?: () => void;
  failUploadOnce = false;
  failList = false;
  failSearch = false;
  extra: Remote[] = [];
  returnedContent = 'UNTRUSTED PROVIDER TEXT';
  items = {
    list: async ({ key }: { key: string }) => {
      if (this.failList) throw new Error('provider unavailable');
      return { result: [...this.records.values()].filter(row => row.key === key) };
    },
    upload: async (key: string, content: string, options: { metadata: Record<string, string> }) => {
      const uploadNumber = ++this.uploads;
      this.uploadStarted?.();
      await this.uploadGate;
      const row = { id: `item-${uploadNumber}`, key, content, metadata: options.metadata, status: 'completed' };
      this.records.set(key, row);
      this.afterUpload?.();
      if (this.failUploadOnce) { this.failUploadOnce = false; throw new Error('accepted remotely, response lost'); }
      return row;
    },
    delete: async (id: string) => {
      this.deletes++;
      if (this.failDelete) throw new Error('private provider details must not enter status');
      for (const [key, row] of this.records) if (row.id === id) this.records.delete(key);
    },
    get: (_id: string) => ({ sync: async () => ({}) }),
  };
  search = async (input: unknown) => {
    this.searches.push(input);
    this.beforeSearch?.();
    if (this.failSearch) throw new Error('provider unavailable');
    return { chunks: [...this.records.values(), ...this.extra].map(item => ({
      item: { metadata: item.metadata }, text: this.returnedContent, score: .9,
    })) };
  };
  get binding() { return this as unknown as AiSearchInstance; }
}
async function fixture(run: (store: MarkdownMemoryStore, storage: DurableObjectStorage, ai: FakeAi) => Promise<void>) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
    await run(new MarkdownMemoryStore(state.storage), state.storage, new FakeAi());
  });
}
const queued = (storage: DurableObjectStorage) => storage.sql.exec<{ chunk_id: number; operation: string; retry_at: number | null }>(
  'SELECT chunk_id,operation,retry_at FROM markdown_memory_ai_items ORDER BY chunk_id',
).toArray();

describe('Markdown semantic retrieval in canonical Workers SQL storage', () => {
  it('recalls paraphrases with the existing binding, with exact scope filters and canonical text', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('USER.md', 'I avoid meals containing animal products.'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      expect(store.search('alice', { query: 'vegan diet' }).results).toEqual([]);
      const result = await semantic.search('alice', { query: 'vegan diet' });
      expect(result).toMatchObject({ retrieval: { mode: 'hybrid', semantic: 'available' }, results: [
        { path: 'USER.md', revision: 1, snippet: 'I avoid meals containing animal products.' },
      ] });
      expect(JSON.stringify(result)).not.toContain(ai.returnedContent);
      expect(ai.searches[0]).toMatchObject({ ai_search_options: { retrieval: { retrieval_type: 'vector', filters: {
        kind: { $eq: 'markdown_memory' }, organization_id: { $eq: 'org-a' }, owner: { $eq: 'alice' },
      } }, cache: { enabled: false } } });
      expect(queued(storage)[0]?.retry_at).toBeNull();
    });
  });
  it('revives the durable queue after no binding and reconstruction, and reconciles ambiguous uploads', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'Copper finch'));
      const offline = new MarkdownMemorySemantic(storage, 'org-a');
      await offline.drain();
      expect(offline.nextRetryAt()).toBeUndefined();
      expect(queued(storage)).toHaveLength(1);
      ai.failUploadOnce = true;
      const first = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await first.drain();
      expect(ai.uploads).toBe(1);
      expect(first.nextRetryAt()).toBeTypeOf('number');
      const restarted = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await restarted.drain(Date.now() + 10_000);
      expect(ai.uploads).toBe(1);
      expect(restarted.nextRetryAt()).toBeUndefined();
      expect((await restarted.search('alice', { query: 'bird' })).results).toHaveLength(1);
    });
  });
  it('rejects cross-owner, cross-organization, malformed, and superseded AI hits', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'Alice original'));
      store.write('bob', put('USER.md', 'Bob private'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      const alice = [...ai.records.values()].find(row => row.metadata.owner === 'alice')!;
      ai.extra = [
        { ...alice, metadata: { ...alice.metadata, organization_id: 'org-b' } },
        { ...alice, metadata: { ...alice.metadata, revision: '9000' } },
        { ...alice, metadata: { ...alice.metadata, markdown_chunk_id: 'NaN' } },
        { ...alice, metadata: { ...alice.metadata, kind: 'history' } },
      ];
      expect((await semantic.search('alice', { query: 'paraphrase' })).results.map(hit => hit.path)).toEqual(['MEMORY.md']);
      store.write('alice', put('MEMORY.md', 'Alice replacement', 1));
      expect((await semantic.search('alice', { query: 'paraphrase' })).results).toEqual([]);
      expect((await semantic.search('charlie', { query: 'paraphrase' })).results).toEqual([]);
    });
  });
  it('revalidates lexical and semantic candidates after a delete during retrieval', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'copper finch'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      ai.beforeSearch = () => { store.write('alice', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 }); };
      expect((await semantic.search('alice', { query: 'copper' })).results).toEqual([]);
      expect(queued(storage)[0]?.operation).toBe('delete');
    });
  });
  it('cleans up uploads that finish during deletion, without reviving the live index', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'forget this'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      ai.afterUpload = () => { store.write('alice', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 }); };
      await semantic.drain();
      expect(queued(storage)[0]).toMatchObject({ operation: 'delete' });
      expect((await semantic.search('alice', { query: 'forget' })).results).toEqual([]);
      await new MarkdownMemorySemantic(storage, 'org-a', ai.binding).drain(Date.now() + 10_000);
      expect(ai.records.size).toBe(0);
      expect(queued(storage)[0]?.retry_at).toBeTypeOf('number');
    });
  });
  it('reports missing/error semantic service explicitly and retains lexical recall', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'copper finch'));
      const offline = await new MarkdownMemorySemantic(storage, 'org-a').search('alice', { query: 'copper' });
      expect(offline).toMatchObject({ results: [{ path: 'MEMORY.md' }], retrieval: { mode: 'fts', semantic: 'unavailable' } });
      ai.failSearch = true;
      const fallback = await new MarkdownMemorySemantic(storage, 'org-a', ai.binding).search('alice', { query: 'copper' });
      expect(fallback).toMatchObject({ results: [{ path: 'MEMORY.md' }], retrieval: { mode: 'fts', semantic: 'error' } });
      await expect(new MarkdownMemorySemantic(storage, 'org-a', ai.binding).search('alice', { query: 'copper', limit: 21 })).rejects.toThrow();
    });
  });
  it('fuses both recall sets, decays dated notes, preserves evergreen notes and diversifies repeated passages', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('memory/2000-01-01.md', 'copper ancient event'));
      store.write('alice', put('USER.md', 'copper evergreen preference'));
      store.write('alice', put('MEMORY.md', Array.from({ length: 90 }, () => 'copper duplicate passage').join('\n')));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      for (let i = 0; i < 3; i++) await semantic.drain();
      const result = await semantic.search('alice', { query: 'copper', limit: 3 });
      expect(result.results[0]?.path).not.toBe('memory/2000-01-01.md');
      expect(new Set(result.results.map(hit => hit.path)).size).toBe(3);
      expect(result.results.every(hit => hit.snippet.length <= 2048)).toBe(true);
    });
  });
  it('migrates preexisting FTS chunks once and uses immutable identities across revisions', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'original'));
      storage.sql.exec('DELETE FROM markdown_memory_ai_items');
      storage.sql.exec("DELETE FROM markdown_memory_migrations WHERE name='ai-items'");
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      expect(queued(storage)).toHaveLength(1);
      await semantic.drain();
      const firstKey = [...ai.records.keys()][0];
      store.write('alice', put('MEMORY.md', 'replacement', 1));
      await semantic.drain();
      expect(queued(storage).map(row => row.operation)).toEqual(['delete', 'upload']);
      expect([...ai.records.keys()]).not.toContain(firstKey);
      expect([...ai.records.values()][0]?.metadata.revision).toBe('2');
    });
  });
  it('ends cleanup sweeps at a finite horizon and retains an owner-scoped receipt across restarts', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'forget this'));
      store.write('bob', put('USER.md', 'other owner'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      store.write('alice', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 });
      expect(semantic.status('alice')).toMatchObject({ indexed: 0, pending: 0, cleanup: { pending: 1, completed: 0 } });
      expect(semantic.status('bob')).toMatchObject({ indexed: 1, cleanup: { pending: 0 } });
      await semantic.drain();
      expect(ai.records.size).toBe(1);
      const deadline = storage.sql.exec<{ cleanup_until: number }>("SELECT cleanup_until FROM markdown_memory_ai_items WHERE owner='alice'").one().cleanup_until;
      const restarted = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await restarted.drain(deadline);
      expect(restarted.nextRetryAt()).toBeUndefined();
      expect(restarted.status('alice')).toMatchObject({ failures: 0, next_retry_at: null,
        cleanup: { pending: 0, completed: 1, expired: 0, horizon_ms: MARKDOWN_MEMORY_CLEANUP_HORIZON_MS } });
      const deletes = ai.deletes;
      await restarted.drain(deadline + MARKDOWN_MEMORY_CLEANUP_HORIZON_MS);
      expect(ai.deletes).toBe(deletes);
      expect(queued(storage)).toHaveLength(2);
      expect(() => restarted.status('')).toThrow('owner');
    });
  });
  it('reports failed cleanup without retrying indefinitely or exposing provider error text', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'private text'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      store.write('alice', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 });
      ai.failDelete = true;
      await semantic.drain();
      expect(semantic.status('alice')).toMatchObject({ failures: 1, cleanup: { pending: 1 } });
      const deadline = storage.sql.exec<{ cleanup_until: number }>('SELECT cleanup_until FROM markdown_memory_ai_items').one().cleanup_until;
      await new MarkdownMemorySemantic(storage, 'org-a', ai.binding).drain(deadline);
      expect(semantic.nextRetryAt()).toBeUndefined();
      expect(semantic.status('alice')).toMatchObject({ failures: 1, cleanup: { pending: 0, expired: 1 } });
      expect(storage.sql.exec<{ last_error: string }>('SELECT last_error FROM markdown_memory_ai_items').one().last_error).toBe('provider_error');
      expect((await semantic.search('alice', { query: 'private' })).results).toEqual([]);
    });
  });
  it('reopens deletion after an upload settles beyond another instance’s cleanup horizon', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'late remote upload'));
      let finish!: () => void, started!: () => void;
      ai.uploadGate = new Promise<void>(resolve => { finish = resolve; });
      const entered = new Promise<void>(resolve => { started = resolve; });
      ai.uploadStarted = started;
      const first = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      const draining = first.drain();
      await entered;
      store.write('alice', { operation: 'delete', path: 'MEMORY.md', expected_revision: 1 });
      const deadline = storage.sql.exec<{ cleanup_until: number }>('SELECT cleanup_until FROM markdown_memory_ai_items').one().cleanup_until;
      const restarted = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await restarted.drain(deadline);
      expect(restarted.status('alice').cleanup.completed).toBe(1);
      finish();
      await draining;
      expect(ai.records.size).toBe(1);
      expect(restarted.status('alice').cleanup).toMatchObject({ pending: 1, completed: 0 });
      expect(first.nextRetryAt()).toBeTypeOf('number');
      expect(await storage.getAlarm()).not.toBeNull();
      expect((await restarted.search('alice', { query: 'remote' })).results).toEqual([]);
      await restarted.drain();
      expect(ai.records.size).toBe(0);
      const finalDeadline = storage.sql.exec<{ cleanup_until: number }>('SELECT cleanup_until FROM markdown_memory_ai_items').one().cleanup_until;
      await restarted.drain(finalDeadline);
      expect(restarted.nextRetryAt()).toBeUndefined();
    });
  });
  it('keeps a durable watchdog across timed-out uploads and reconstruction', async () => {
    await fixture(async (store, storage, ai) => {
      for (const path of ['MEMORY.md', 'USER.md', 'memory/2026-09-21.md', 'memory/2026-09-22.md']) {
        store.write('alice', put(path, 'delayed upload'));
      }
      let finish!: () => void;
      ai.uploadGate = new Promise<void>(resolve => { finish = resolve; });
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain(); // Deliberately exceeds the bounded RPC wait.
      expect(semantic.status('alice')).toMatchObject({ pending: 4, failures: 4, exhausted: 0 });
      const next = semantic.nextRetryAt()!;
      expect(next).toBeGreaterThan(Date.now());
      expect(await storage.getAlarm()).not.toBeNull();
      const restarted = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      expect(restarted.nextRetryAt()).toBe(next);
      await restarted.drain();
      expect(ai.uploads).toBe(4); // All four persisted leases still exclude overlap.
      finish();
      await vi.waitFor(() => expect(semantic.status('alice')).toMatchObject({ pending: 0, indexed: 4, failures: 0 }));
      expect(semantic.nextRetryAt()).toBeUndefined();
    });
  }, 15_000);
  it('exhausts an upload retry budget durably and revives a new canonical revision', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'queued fact'));
      ai.failList = true;
      let semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      const budget = semantic.status('alice').attempt_budget;
      for (let i = 0; i < budget; i++) {
        await semantic.drain(semantic.nextRetryAt()!);
        semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      }
      await semantic.drain(semantic.nextRetryAt()!);
      expect(semantic.nextRetryAt()).toBeUndefined();
      expect(semantic.status('alice')).toMatchObject({ pending: 0, indexed: 0, failures: 1, exhausted: 1 });
      expect(ai.uploads).toBe(0);
      expect(JSON.stringify(storage.sql.exec('SELECT * FROM markdown_memory_ai_items').toArray())).not.toContain('queued fact');
      store.write('alice', put('MEMORY.md', 'replacement fact', 1));
      ai.failList = false;
      await semantic.drain();
      expect(semantic.status('alice')).toMatchObject({ indexed: 1, exhausted: 0 });
      expect(ai.uploads).toBe(1);
    });
  });
  it('allows DREAMS as a journal without exposing it through any recall projection', async () => {
    await fixture(async (store, storage, ai) => {
      expect(store.write('alice', put('DREAMS.md', 'journal secret copper'))).toMatchObject({ ok: true });
      expect(store.list('alice')).toContain('DREAMS.md');
      expect(store.get('alice', { path: 'DREAMS.md' }).content).toBe('journal secret copper');
      expect(store.search('alice', { query: 'copper' }).results).toEqual([]);
      expect(store.bootstrap('alice', Date.now()).documents).toEqual([]);
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      expect(ai.uploads).toBe(0);
      expect(queued(storage)).toEqual([]);
      expect((await semantic.search('alice', { query: 'journal' })).results).toEqual([]);
    });
  });
  it('rehydrates only matching canonical revisions, even when an old derived index remains', async () => {
    await fixture(async (store, storage, ai) => {
      store.write('alice', put('MEMORY.md', 'stale copper projection'));
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      await semantic.drain();
      storage.sql.exec("UPDATE markdown_memory_documents SET revision=2,content='current fact' WHERE owner='alice'");
      expect(store.search('alice', { query: 'copper' }).results).toEqual([]);
      expect((await semantic.search('alice', { query: 'copper' })).results).toEqual([]);
      storage.sql.exec("UPDATE markdown_memory_documents SET revision=1,deleted=1 WHERE owner='alice'");
      expect(store.search('alice', { query: 'copper' }).results).toEqual([]);
      expect((await semantic.search('alice', { query: 'copper' })).results).toEqual([]);
    });
  });
  it('upgrades old indefinite deletion receipts without resetting their horizon on restart', async () => {
    await fixture(async (_store, storage, ai) => {
      storage.sql.exec(`DROP TABLE markdown_memory_ai_items;
        CREATE TABLE markdown_memory_ai_items(chunk_id INTEGER PRIMARY KEY,owner TEXT NOT NULL,path TEXT NOT NULL,
          revision INTEGER NOT NULL,operation TEXT NOT NULL,item_id TEXT,attempts INTEGER NOT NULL DEFAULT 0,retry_at INTEGER);
        INSERT INTO markdown_memory_ai_items VALUES(1,'alice','MEMORY.md',1,'delete',NULL,9,0);
        DELETE FROM markdown_memory_migrations WHERE name='ai-cleanup-v2';`);
      const semantic = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      const deadline = storage.sql.exec<{ cleanup_until: number }>('SELECT cleanup_until FROM markdown_memory_ai_items').one().cleanup_until;
      expect(deadline).toBeGreaterThan(Date.now());
      await semantic.drain(deadline);
      const restarted = new MarkdownMemorySemantic(storage, 'org-a', ai.binding);
      expect(restarted.nextRetryAt()).toBeUndefined();
      expect(restarted.status('alice').cleanup.completed).toBe(1);
      expect(storage.sql.exec<{ cleanup_until: number }>('SELECT cleanup_until FROM markdown_memory_ai_items').one().cleanup_until).toBe(deadline);
    });
  });
  it('rolls back outbox mutations together with canonical body/index failure', async () => {
    await fixture(async (store, storage) => {
      store.write('alice', put('MEMORY.md', 'original'));
      const before = queued(storage);
      storage.sql.exec('DROP TABLE markdown_memory_fts');
      expect(() => store.write('alice', put('MEMORY.md', 'replacement', 1))).toThrow();
      expect(queued(storage)).toEqual(before);
      expect(store.readFile('alice', 'MEMORY.md')).toBe('original');
    });
  });
});
