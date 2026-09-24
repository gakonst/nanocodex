import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MarkdownMemoryStore } from '../src/markdown-memory';
async function withStore(run: (store: MarkdownMemoryStore, storage: DurableObjectStorage) => void) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => run(new MarkdownMemoryStore(state.storage), state.storage));
}
const put = (path: string, content: string, expected_revision = 0) => ({ operation: 'put', path, content, expected_revision });
describe('canonical markdown memory', () => {
  it('appends once across store reconstruction and rejects stale or mismatched retries', async () => {
    await withStore((store, storage) => {
      const input = { operation: 'append', path: 'memory/2026-09-22-topic.md', content: 'First event', operation_id: 'event-1', expected_revision: 0 };
      expect(store.write('alice', input)).toMatchObject({ ok: true, revision: 1 });
      expect(new MarkdownMemoryStore(storage).write('alice', input)).toMatchObject({ ok: true, revision: 1, replayed: true });
      expect(() => store.write('alice', { ...input, content: 'different' })).toThrow('different input');
      expect(store.write('alice', { ...input, operation_id: 'event-2' })).toMatchObject({ ok: false, revision: 1 });
      store.write('alice', { ...input, operation_id: 'event-2', expected_revision: 1, content: 'Second event' });
      expect(store.get('alice', { path: input.path }).content).toBe('First event\nSecond event');
      store.write('alice', { operation: 'delete', path: input.path, expected_revision: 2 });
      expect(store.write('alice', input)).toMatchObject({ replayed: true, revision: 1 });
      expect(store.get('alice', { path: input.path }).deleted).toBe(true);
    });
  });
  it('validates paths, dates, revisions, byte limits, and unknown input fields', async () => {
    await withStore(store => {
      for (const path of ['../MEMORY.md', '/MEMORY.md', 'memory/2026-02-30.md', 'memory/2026-09-22/evil.md', 'memory/2026-09-22-Upper.md', 'other.md']) {
        expect(() => store.write('alice', put(path, 'x'))).toThrow();
      }
      for (const input of [null, [], { ...put('USER.md', 'x'), extra: true }, put('USER.md', 'é'.repeat(32769)), { ...put('USER.md', 'x'), expected_revision: -1 }]) {
        expect(() => store.write('alice', input)).toThrow();
      }
      expect(() => store.get('', { path: 'USER.md' })).toThrow();
      expect(() => store.search('alice', { query: 'x', limit: 21 })).toThrow();
      expect(store.search('alice', { query: '" OR * ()' }).results).toEqual([]);
      store.write('alice', put('USER.md', ('x'.repeat(8191) + '\n').repeat(8)));
      expect(store.readFile('alice', 'USER.md').length).toBe(65536);
      expect(store.get('alice', { path: 'USER.md' })).toMatchObject({ next_line: 3 });
      expect(() => store.write('alice', put('MEMORY.md', 'x'.repeat(8193)))).toThrow('lines must not exceed');
    });
  });
  it('bounds line reads with revision-aware continuation and bootstrap selection', async () => {
    await withStore(store => {
      store.write('alice', put('MEMORY.md', Array.from({ length: 250 }, (_, i) => `line ${i + 1}`).join('\n')));
      expect(store.get('alice', { path: 'MEMORY.md', from_line: 2, max_lines: 2 })).toMatchObject({ content: 'line 2\nline 3', next_line: 4, total_lines: 250 });
      expect(() => store.get('alice', { path: 'MEMORY.md', revision: 0 })).toThrow('revision changed');
      try { store.get('alice', { path: 'MEMORY.md', revision: 0 }); } catch (error) {
        expect(error).toMatchObject({ status: 409, code: 'revision_conflict' });
      }
      store.write('alice', put('memory/2026-09-22.md', 'today'));
      store.write('alice', put('memory/2026-09-21.md', 'yesterday'));
      store.write('alice', put('memory/2026-09-20.md', 'old'));
      store.write('bob', put('USER.md', 'private'));
      const boot = store.bootstrap('alice', Date.parse('2026-09-22T00:01:00Z'));
      expect(boot.documents.map(doc => doc.path)).toEqual(['MEMORY.md', 'memory/2026-09-22.md', 'memory/2026-09-21.md']);
      expect(boot.documents[0]?.truncated).toBe(true);
    });
  });
  it('bounds bootstrap bytes and stores only hashes in append receipts', async () => {
    await withStore((store, storage) => {
      for (const path of ['MEMORY.md', 'USER.md', 'memory/2026-09-22.md', 'memory/2026-09-21.md']) {
        store.write('alice', put(path, 'é'.repeat(4096)));
      }
      const bootstrap = store.bootstrap('alice', Date.parse('2026-09-22T00:01:00Z'));
      expect(bootstrap.documents.reduce((sum, doc) => sum + new TextEncoder().encode(doc.content).length, 0)).toBeLessThanOrEqual(12288);
      expect(Object.keys(bootstrap.documents[0]!).sort()).toEqual(['content', 'path', 'revision', 'truncated']);
      store.write('alice', { operation: 'append', path: 'memory/2026-09-22-topic.md', expected_revision: 0, operation_id: 'receipt', content: 'forgotten prose' });
      const receipt = storage.sql.exec<{ request: string; result: string }>('SELECT request,result FROM markdown_memory_operations').one();
      expect(receipt.request).toMatch(/^[a-f0-9]{64}$/);
      expect(JSON.stringify(receipt)).not.toContain('forgotten prose');
      store.write('alice', put('memory/2026-09-22-boundary.md', ' '.repeat(2044) + 'copperfinch'));
      expect(store.search('alice', { query: 'copperfinch' }).results.length).toBeGreaterThan(0);
    });
  });
  it('uses an owner/path index to replace one document amid unrelated search chunks', async () => {
    await withStore((store, storage) => {
      store.write('alice', put('MEMORY.md', 'original copper'));
      for (let i = 0; i < 20; i++) {
        store.write('bob', put(`memory/2026-09-22-topic-${i}.md`, ('unrelated '.repeat(400) + '\n').repeat(8)));
      }
      const otherChunks = storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM markdown_memory_chunks WHERE owner='bob'",
      ).one().count;
      expect(otherChunks).toBeGreaterThan(100);
      const lookup = storage.sql.exec<{ id: number }>(
        'SELECT id FROM markdown_memory_chunks WHERE owner=? AND path=?', 'alice', 'MEMORY.md',
      );
      expect(lookup.toArray()).toHaveLength(1);
      expect(lookup.rowsRead).toBeLessThanOrEqual(2);
      const plan = storage.sql.exec<{ detail: string }>(`EXPLAIN QUERY PLAN DELETE FROM markdown_memory_fts
        WHERE rowid IN (SELECT id FROM markdown_memory_chunks WHERE owner=? AND path=?)`, 'alice', 'MEMORY.md')
        .toArray().map(row => row.detail).join('\n');
      expect(plan).toContain('USING COVERING INDEX markdown_memory_chunks_owner_path');
      // FTS5's '=' strategy is a rowid lookup; an unconstrained virtual scan has no '='.
      expect(plan).toMatch(/markdown_memory_fts VIRTUAL TABLE INDEX [^\n]*=/);
      store.write('alice', put('MEMORY.md', 'replacement heron', 1));
      expect(store.search('alice', { query: 'copper' }).results).toEqual([]);
      expect(store.search('alice', { query: 'heron' }).results).toHaveLength(1);
      expect(storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM markdown_memory_chunks WHERE owner='bob'",
      ).one().count).toBe(otherChunks);
    });
  });
  it('rolls back both body and index if indexing fails, and leaves rejected appends unrecorded', async () => {
    await withStore((store, storage) => {
      store.write('alice', put('MEMORY.md', 'original copper'));
      storage.sql.exec('DROP TABLE markdown_memory_fts');
      expect(() => store.write('alice', put('MEMORY.md', 'replacement', 1))).toThrow();
      expect(store.get('alice', { path: 'MEMORY.md' })).toMatchObject({ revision: 1, content: 'original copper' });
      const restored = new MarkdownMemoryStore(storage);
      restored.write('alice', put('memory/2026-09-22.md', ('x'.repeat(8191) + '\n').repeat(8)));
      expect(() => restored.write('alice', { operation: 'append', path: 'memory/2026-09-22.md', expected_revision: 1, operation_id: 'overflow', content: 'overflow' })).toThrow();
      expect(storage.sql.exec('SELECT * FROM markdown_memory_operations').toArray()).toEqual([]);
      expect(restored.get('alice', { path: 'memory/2026-09-22.md' }).revision).toBe(1);
    });
  });
});
