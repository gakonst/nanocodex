import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MarkdownMemoryStore } from '../src/markdown-memory';
import { MarkdownMemorySemantic } from '../src/markdown-memory-semantic';

const put = (path: string, content: string, expected_revision = 0) => ({ operation: 'put', path, content, expected_revision });
type Remote = { id: string; key: string; status: string; metadata: Record<string, string>; content: string };
class FakeAi {
  records = new Map<string, Remote>();
  uploads = 0;
  searches: unknown[] = [];
  afterUpload?: () => void;
  beforeSearch?: () => void;
  failUploadOnce = false;
  failSearch = false;
  extra: Remote[] = [];
  returnedContent = 'UNTRUSTED PROVIDER TEXT';
  items = {
    list: async ({ key }: { key: string }) => ({ result: [...this.records.values()].filter(row => row.key === key) }),
    upload: async (key: string, content: string, options: { metadata: Record<string, string> }) => {
      this.uploads++;
      const row = { id: `item-${this.uploads}`, key, content, metadata: options.metadata, status: 'completed' };
      this.records.set(key, row);
      this.afterUpload?.();
      if (this.failUploadOnce) { this.failUploadOnce = false; throw new Error('accepted remotely, response lost'); }
      return row;
    },
    delete: async (id: string) => { for (const [key, row] of this.records) if (row.id === id) this.records.delete(key); },
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
