import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MarkdownMemoryStore } from '../src/markdown-memory';
import { MarkdownMemoryConsolidation, type MemoryChangeOrigin } from '../src/markdown-memory-consolidation';
import type { MarkdownMemoryCompletion, MarkdownMemoryCompletionRequest } from '../src/markdown-memory-ai';

const DAY = 86_400_000;
const START = Date.parse('2026-09-22T12:00:00Z');
const PATH = 'memory/2026-09-22.md';
type Source = { path: string; revision: number; from_line: number; to_line: number; content: string };
type Input = { sources: Source[]; curated: Record<string, string>; managed_entries: { id: string; target: string }[] };
function selected(request: MarkdownMemoryCompletionRequest, target = 'USER.md', sourceIndex = 0) {
  const source = (request.input as Input).sources[sourceIndex]!;
  return { target, quote: source.content.split('\n')[0]!, replace_ids: [] as string[], sources: [{
    path: source.path, revision: source.revision, from_line: source.from_line, to_line: source.from_line,
  }] };
}
function selectedLine(request: MarkdownMemoryCompletionRequest, line: number) {
  const candidate = selected(request);
  const source = (request.input as Input).sources[0]!;
  candidate.quote = source.content.split('\n')[line - source.from_line]!;
  candidate.sources[0]!.from_line = candidate.sources[0]!.to_line = line;
  return candidate;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
async function fixture(run: (h: {
  storage: DurableObjectStorage; store: MarkdownMemoryStore;
  create: (complete?: MarkdownMemoryCompletion) => MarkdownMemoryConsolidation;
  change: (service: MarkdownMemoryConsolidation, path: string, content: string | null, owner?: string, origin?: MemoryChangeOrigin) => number;
  advance: (time: number) => void;
}) => Promise<void>) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
    let now = START;
    const storage = state.storage;
    const store = new MarkdownMemoryStore(storage);
    const create = (complete: MarkdownMemoryCompletion = async request => ({ candidates: [selected(request)] })) =>
      new MarkdownMemoryConsolidation(storage, new MarkdownMemoryStore(storage), {
        complete, containsSecret: text => text.includes('FIXTURE_SECRET'), now: () => now,
      });
    const change = (service: MarkdownMemoryConsolidation, path: string, content: string | null, owner = 'alice', origin: MemoryChangeOrigin = 'direct') =>
      storage.transactionSync(() => {
        const revision = store.get(owner, { path }).revision;
        const result = store.write(owner, content === null
          ? { operation: 'delete', path, expected_revision: revision }
          : { operation: 'put', path, expected_revision: revision, content });
        if (!result.ok) throw new Error('unexpected fixture conflict');
        service.noteChange(owner, path, result.revision, origin);
        return result.revision;
      });
    await run({ storage, store, create, change, advance: time => { now = time; } });
  });
}

describe('durable daily markdown consolidation', () => {
  it('uses the shared tool-free schema and survives reconstruction with exact provenance and audit preimages', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const complete = vi.fn<MarkdownMemoryCompletion>(async request => ({ candidates: [selected(request)] }));
      let service = create(complete);
      change(service, 'USER.md', 'Hand curated preference.');
      change(service, PATH, 'I prefer concise answers.\nTransient chatter.');
      change(service, PATH, 'Bob only fact.', 'bob');
      expect(await service.runDue()).toBeNull();
      expect(service.nextAlarm()).toBe(Date.parse('2026-09-23T00:00:00Z'));
      service = create(complete);
      advance(START + DAY);
      const result = await service.runDue();
      expect(result).toMatchObject({ owner: 'alice', status: 'committed', sources: 1, additions: 1 });
      const request = complete.mock.calls[0]![0];
      expect(Object.keys(request).sort()).toEqual(['input', 'schema', 'signal', 'system']);
      expect(request.schema).toMatchObject({ additionalProperties: false, required: ['candidates'] });
      expect(JSON.stringify(request.input)).not.toContain('Bob only');
      expect(store.readFile('alice', 'USER.md')).toContain('Hand curated preference.');
      expect(store.readFile('alice', 'USER.md')).toContain('> I prefer concise answers.');
      const entry = storage.sql.exec<{ sources: string }>('SELECT sources FROM markdown_consolidation_entries').one();
      expect(JSON.parse(entry.sources)).toEqual([{ path: PATH, revision: 1, from_line: 1, to_line: 1 }]);
      expect(storage.sql.exec<{ content: string }>("SELECT content FROM markdown_consolidation_preimages WHERE path='USER.md'").one().content).toBe('Hand curated preference.');
      expect(store.readFile('alice', 'DREAMS.md')).toContain(result!.id);
      expect(store.search('alice', { query: 'consolidation' }).results.every(row => row.path !== 'DREAMS.md')).toBe(true);
      expect(create().status('alice')).toMatchObject({ pending: [], next_at: null, receipts: [result] });
      expect(service.status('bob').pending).toHaveLength(1);
    });
  });

  it('does not reinforce repeated quotes across paths or targets, or create a recalled/source loop', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      let service = create();
      change(service, PATH, 'I prefer concise answers.');
      advance(START + DAY);
      await service.runDue();
      const revision = store.get('alice', { path: 'USER.md' }).revision;
      service = create(async request => ({ candidates: [selected(request, 'MEMORY.md')] }));
      change(service, 'memory/2026-09-23.md', 'I prefer concise answers.');
      advance(START + 2 * DAY);
      expect(await service.runDue()).toMatchObject({ status: 'empty', additions: 0 });
      expect(store.get('alice', { path: 'USER.md' }).revision).toBe(revision);
      expect(store.get('alice', { path: 'MEMORY.md' }).deleted).toBe(true);
      expect(storage.sql.exec('SELECT id FROM markdown_consolidation_entries').toArray()).toHaveLength(1);
      const complete = vi.fn<MarkdownMemoryCompletion>(async () => ({ candidates: [] }));
      service = create(complete);
      change(service, 'memory/2026-09-24.md', 'retrieval copy', 'alice', 'recalled');
      change(service, 'memory/2026-09-24-copy.md', 'derived copy', 'alice', 'consolidation');
      expect(service.nextAlarm()).toBeNull();
      change(service, 'memory/2026-09-24-marked.md', '<memory_context>retrieval copy</memory_context>');
      advance(START + 3 * DAY);
      expect(await service.runDue()).toMatchObject({ status: 'empty' });
      expect(complete).not.toHaveBeenCalled();
      expect(service.nextAlarm()).toBeNull();
    });
  });

  it('deleting a source removes attributed entries, every retained preimage, and search text without harming other owners', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const service = create();
      change(service, 'USER.md', 'Keep this curated line.');
      change(service, PATH, 'Copperfinch forgotten preference.');
      change(service, PATH, 'Copperfinch belongs to bob.', 'bob');
      advance(START + DAY);
      await service.runDue();
      await service.runDue();
      expect(storage.sql.exec("SELECT * FROM markdown_consolidation_preimages WHERE owner='alice'").toArray().length).toBeGreaterThan(0);
      change(service, PATH, null);
      expect(store.readFile('alice', 'USER.md')).toContain('Keep this curated line.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('Copperfinch');
      expect(store.search('alice', { query: 'Copperfinch' }).results).toEqual([]);
      expect(storage.sql.exec("SELECT * FROM markdown_consolidation_entries WHERE owner='alice'").toArray()).toEqual([]);
      expect(storage.sql.exec("SELECT * FROM markdown_consolidation_preimages WHERE owner='alice'").toArray()).toEqual([]);
      expect(store.readFile('bob', 'USER.md')).toContain('Copperfinch belongs to bob.');
      create().noteChange('alice', PATH, 1); // An old successful append receipt cannot requeue a tombstone.
      expect(create().status('alice').pending).toEqual([]);
    });
  });

  it.each(['append', 'put'])('retains exact evidence and refreshes citations after an unrelated daily %s', async operation => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const service = create();
      change(service, PATH, 'I prefer concise answers.\nOld unrelated detail.');
      advance(START + DAY);
      await service.runDue();
      storage.transactionSync(() => {
        const result = store.write('alice', { operation, path: PATH, expected_revision: 1,
          content: operation === 'append' ? 'New daily detail.' : 'I prefer concise answers.\nCorrected unrelated detail.' });
        expect(result).toMatchObject({ ok: true, revision: 2 });
        create().noteChange('alice', PATH, result.revision);
      });
      expect(store.readFile('alice', 'USER.md')).toContain('> I prefer concise answers.');
      expect(store.search('alice', { query: 'concise' }).results.map(row => row.path)).toContain('USER.md');
      const entry = storage.sql.exec<{ sources: string; rendered: string }>('SELECT sources,rendered FROM markdown_consolidation_entries').one();
      expect(JSON.parse(entry.sources)).toEqual([{ path: PATH, revision: 2, from_line: 1, to_line: 1 }]);
      expect(store.readFile('alice', 'USER.md')).toContain(entry.rendered);
      expect(entry.rendered).toContain('"revision":2');
      expect(create().status('alice').pending).toMatchObject([{ path: PATH, revision: 2 }]);
    });
  });

  it('retracts only managed facts whose exact source spans changed', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const service = create(async request => {
        const first = selected(request);
        return { candidates: [first, { ...first, target: 'MEMORY.md', quote: 'The launch month is June.',
          sources: [{ ...first.sources[0]!, from_line: 2, to_line: 2 }] }] };
      });
      change(service, PATH, 'I prefer concise answers.\nThe launch month is June.');
      advance(START + DAY);
      expect(await service.runDue()).toMatchObject({ additions: 2 });
      change(service, PATH, 'I prefer concise answers.\nThe launch month is July.');
      expect(store.readFile('alice', 'USER.md')).toContain('I prefer concise answers.');
      expect(store.readFile('alice', 'MEMORY.md')).not.toContain('June');
      expect(store.search('alice', { query: 'June' }).results).toEqual([]);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_entries').toArray()).toHaveLength(1);
    });
  });

  it('retracts a multi-source fact when any cited span is corrected', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const other = 'memory/2026-09-23.md';
      const service = create(async request => {
        const a = selected(request), b = selected(request, 'USER.md', 1);
        return { candidates: [{ ...a, quote: a.quote + '\n' + b.quote, sources: [...a.sources, ...b.sources] }] };
      });
      change(service, PATH, 'The launch month is June.');
      change(service, other, 'The release branch is amber.');
      advance(START + DAY);
      expect(await service.runDue()).toMatchObject({ additions: 1 });
      change(service, PATH, 'The launch month is June.\nUnrelated new line.');
      expect(store.readFile('alice', 'USER.md')).toContain('June');
      change(service, other, 'The release branch is jade.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('June');
      expect(store.readFile('alice', 'USER.md')).not.toContain('amber');
    });
  });

  it.each(['USER.md', 'MEMORY.md', 'DREAMS.md'])('editing %s preserves unclaimed daily work across reconstruction', async target => {
    await fixture(async ({ create, change, advance, store }) => {
      const service = create();
      change(service, PATH, 'Unrelated durable preference.');
      const before = service.status('alice');
      change(service, target, 'Manual curated note.');
      const restored = create();
      expect(restored.status('alice')).toEqual(before);
      advance(START + DAY);
      expect(await restored.runDue()).toMatchObject({ status: 'committed', additions: 1 });
      expect(store.readFile('alice', 'USER.md')).toContain('Unrelated durable preference.');
      expect(store.readFile('alice', target)).toContain('Manual curated note.');
    });
  });

  it('manual curation fences a late completion while retaining its inputs and unrelated pending notes', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, 'Pending preference.');
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      const fresh = 'memory/2026-09-23.md';
      change(create(), fresh, 'Fresh independent fact.');
      change(create(), 'USER.md', 'Manual preference.');
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'stale', reason: 'claim_replaced' });
      expect(store.readFile('alice', 'USER.md')).toBe('Manual preference.');
      expect(create().status('alice').pending.map(event => event.path)).toEqual([PATH, fresh]);
      advance(START + 2 * DAY);
      const complete = vi.fn<MarkdownMemoryCompletion>(async request => ({ candidates: [selected(request, 'MEMORY.md', 1)] }));
      expect(await create(complete).runDue()).toMatchObject({ status: 'committed', additions: 1 });
      expect((complete.mock.calls[0]![0].input as Input).curated['USER.md']).toBe('Manual preference.');
      expect(store.readFile('alice', 'MEMORY.md')).toContain('Fresh independent fact.');
    });
  });

  it('removing a managed fact preserves appended lines in its source and other pending files', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const service = create();
      change(service, PATH, 'Deliberately removed fact.');
      advance(START + DAY);
      await service.runDue();
      // The next revision still contains the original fact and is queued again.
      change(service, PATH, 'Deliberately removed fact.\nAdditional source detail.');
      const other = 'memory/2026-09-23.md';
      change(service, other, 'Unrelated retained fact.');
      change(service, 'USER.md', null);
      expect(create().status('alice').pending.map(event => event.path)).toEqual([PATH, other]);
      advance(START + 2 * DAY);
      const complete = vi.fn<MarkdownMemoryCompletion>(async request => ({
        candidates: [selectedLine(request, 2), selected(request, 'USER.md', 1)],
      }));
      expect(await create(complete).runDue()).toMatchObject({ status: 'committed', additions: 2 });
      expect(JSON.stringify(complete.mock.calls[0]![0].input)).not.toContain('Deliberately removed fact.');
      expect(store.readFile('alice', 'USER.md')).toContain('Additional source detail.');
      expect(store.readFile('alice', 'USER.md')).toContain('Unrelated retained fact.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('Deliberately removed fact.');
    });
  });

  it('removing a managed fact preserves an already pending continuation from its source', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      change(create(), PATH, ['Removed fact.', ...Array.from({ length: 63 }, (_, i) => `Earlier detail ${i}.`), 'Retained continuation.'].join('\n'));
      advance(START + DAY);
      expect(await create().runDue()).toMatchObject({ status: 'committed' });
      expect(create().status('alice').pending).toEqual([{ path: PATH, revision: 1, next_line: 65 }]);
      change(create(), 'USER.md', null);
      expect(create().status('alice').pending).toEqual([{ path: PATH, revision: 1, next_line: 65 }]);
      advance(START + 2 * DAY);
      expect(await create().runDue()).toMatchObject({ status: 'committed', additions: 1 });
      expect(store.readFile('alice', 'USER.md')).toContain('Retained continuation.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('Removed fact.');
    });
  });

  it('removing a fact preserves newly edited lines before and after its evidence', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      change(create(), PATH, 'Old prefix.\nRemoved fact.\nOld tail.');
      advance(START + DAY);
      await create(async request => ({ candidates: [selectedLine(request, 2)] })).runDue();
      change(create(), PATH, 'New prefix.\nRemoved fact.\nNew tail.');
      change(create(), 'USER.md', null);
      advance(START + 2 * DAY);
      // A quote crossing the blanked line is not exact canonical evidence.
      const invalid = create(async request => {
        const candidate = selected(request);
        candidate.quote = (request.input as Input).sources[0]!.content;
        candidate.sources[0]!.to_line = 3;
        return { candidates: [candidate] };
      });
      expect(await invalid.runDue()).toMatchObject({ status: 'failed', reason: 'retry_scheduled' });
      expect(store.get('alice', { path: 'USER.md' }).deleted).toBe(true);
      advance(START + 3 * DAY);
      const complete = vi.fn<MarkdownMemoryCompletion>(async request => ({
        candidates: [selectedLine(request, 1), selectedLine(request, 3)],
      }));
      expect(await create(complete).runDue()).toMatchObject({ status: 'committed', additions: 2 });
      expect(JSON.stringify(complete.mock.calls[0]![0].input)).not.toContain('Removed fact.');
      expect(store.readFile('alice', 'USER.md')).toContain('New prefix.');
      expect(store.readFile('alice', 'USER.md')).toContain('New tail.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('Removed fact.');
    });
  });

  it('retains existing continuation cursors when upgrading the event table', async () => {
    await fixture(async ({ create, storage }) => {
      storage.sql.exec(`DROP TABLE IF EXISTS markdown_consolidation_events;
        CREATE TABLE markdown_consolidation_events (
          owner TEXT NOT NULL,path TEXT NOT NULL,revision INTEGER NOT NULL,next_line INTEGER NOT NULL,
          PRIMARY KEY(owner,path));`);
      storage.sql.exec('INSERT INTO markdown_consolidation_events VALUES(?,?,?,?)', 'alice', PATH, 7, 65);
      expect(create().status('alice').pending).toEqual([{ path: PATH, revision: 7, next_line: 65 }]);
      expect(storage.sql.exec('SELECT excluded_spans FROM markdown_consolidation_events').one()).toEqual({ excluded_spans: '[]' });
    });
  });

  it('manual edits preserve the daily inference budget', async () => {
    await fixture(async ({ create, change, advance, storage }) => {
      change(create(), PATH, 'Retained source.');
      advance(START + DAY);
      const complete = vi.fn<MarkdownMemoryCompletion>(async () => { throw new Error('transient completion failure'); });
      const service = create(complete);
      for (let attempt = 0; attempt < 3; attempt++) {
        expect(await service.runDue()).toMatchObject({ status: 'failed' });
        change(create(), 'USER.md', `Manual edit ${attempt}.`);
        // Make the retained work due on the same day, without changing budget fields.
        storage.sql.exec('UPDATE markdown_consolidation_jobs SET due=?,attempts=0 WHERE owner=?', START + DAY, 'alice');
      }
      expect(await service.runDue()).toBeNull();
      expect(complete).toHaveBeenCalledTimes(3);
      expect(storage.sql.exec('SELECT calls,budget_day FROM markdown_consolidation_jobs').one())
        .toEqual({ calls: 3, budget_day: Math.floor((START + DAY) / DAY) });
    });
  });

  it('source corrections immediately remove old derived recall and preserve manually edited blocks', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const service = create();
      change(service, PATH, 'The launch month is June.');
      advance(START + DAY);
      await service.runDue();
      expect(store.readFile('alice', 'USER.md')).toContain('June');
      change(service, PATH, 'The launch month is July.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('June');
      expect(store.search('alice', { query: 'June' }).results).toEqual([]);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray()).toEqual([]);
      advance(START + 2 * DAY);
      await create().runDue();
      const body = store.readFile('alice', 'USER.md');
      change(create(), 'USER.md', body.replace('The launch month is July.', 'Manually selected launch month is August.'));
      change(create(), PATH, null);
      expect(store.readFile('alice', 'USER.md')).toContain('Manually selected launch month is August.');
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_entries').toArray()).toEqual([]);
    });
  });

  it.each(['USER.md', 'MEMORY.md', 'DREAMS.md'])('manual deletion of %s fences in-flight output and lets a new job see edited targets', async target => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, target, 'Curated content.');
      const revision = change(service, PATH, 'Pending source for fresh evaluation.');
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      change(create(), target, null);
      reply.resolve({ candidates: [selected(request, target === 'MEMORY.md' ? target : 'USER.md')] });
      expect(await running).toMatchObject({ status: 'stale', reason: 'claim_replaced' });
      const restored = create();
      restored.noteChange('alice', PATH, revision);
      advance(START + 2 * DAY);
      expect(restored.status('alice').pending).toMatchObject([{ path: PATH, revision }]);
      expect(store.get('alice', { path: target }).deleted).toBe(true);
      const complete = vi.fn<MarkdownMemoryCompletion>(async () => ({ candidates: [] }));
      expect(await create(complete).runDue()).toMatchObject({ status: 'empty' });
      if (target !== 'DREAMS.md') expect((complete.mock.calls[0]![0].input as Input).curated[target]).toBe('');
      expect(store.get('alice', { path: 'USER.md' }).content).not.toContain('Pending source');
    });
  });

  it('manual edits fence in-flight work without losing unprocessed continuation lines', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, Array.from({ length: 100 }, (_, i) => `Old fact ${i}.`).join('\n'));
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      change(create(), 'USER.md', 'Only the manual preference remains.');
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'stale' });
      expect(create().status('alice').pending).toMatchObject([{ path: PATH, revision: 1, next_line: 1 }]);
      expect(store.readFile('alice', 'USER.md')).toBe('Only the manual preference remains.');
    });
  });

  it('consumes conflicted old snapshots without a retry resurrection even if a host misses noteChange', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, Array.from({ length: 100 }, (_, i) => `Old fact ${i}.`).join('\n'));
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      store.write('alice', { operation: 'put', path: 'USER.md', expected_revision: 0, content: 'Concurrent edit.' });
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'conflict', reason: 'target_changed' });
      expect(create().status('alice').pending).toEqual([]);
      expect(store.readFile('alice', 'USER.md')).toBe('Concurrent edit.');
    });
  });

  it.each([false, true])('fences a deleted in-flight source (other pending source: %s)', async otherSource => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, 'Forgotten while model runs.');
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      if (otherSource) change(create(), 'memory/2026-09-23.md', 'Fresh independent fact.');
      change(create(), PATH, null);
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'stale' });
      expect(store.get('alice', { path: 'USER.md' }).deleted).toBe(true);
      expect(JSON.stringify(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray())).not.toContain('Forgotten');
      if (otherSource) {
        advance(START + 2 * DAY);
        expect(await create().runDue()).toMatchObject({ status: 'committed' });
        expect(store.readFile('alice', 'USER.md')).toContain('Fresh independent fact.');
      }
    });
  });

  it('keeps a new source revision queued when an older completion becomes stale', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, 'Old preference.');
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      change(create(), PATH, 'New preference.');
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'stale', reason: 'source_changed' });
      expect(create().status('alice').pending).toMatchObject([{ revision: 2, next_line: 1 }]);
      advance(START + 2 * DAY);
      expect(await create().runDue()).toMatchObject({ status: 'committed' });
      expect(store.readFile('alice', 'USER.md')).toContain('New preference.');
      expect(store.readFile('alice', 'USER.md')).not.toContain('Old preference.');
    });
  });

  it('bounds failed and cancelled inference attempts across reconstruction without retaining model output', async () => {
    await fixture(async ({ create, change, advance, storage, store }) => {
      const controller = new AbortController();
      const entered = deferred<void>();
      const reply = deferred<unknown>();
      let service = create(async () => { entered.resolve(); return reply.promise; });
      change(service, PATH, 'I prefer concise answers.');
      advance(START + DAY);
      const running = service.runDue(controller.signal);
      await entered.promise;
      controller.abort();
      expect(await running).toMatchObject({ status: 'failed', reason: 'retry_scheduled' });
      reply.resolve({ candidates: [] });
      const invalid = vi.fn<MarkdownMemoryCompletion>(async () => ({ candidates: [{ quote: 'FIXTURE_SECRET hostile response' }] }));
      for (let attempt = 0; attempt < 2; attempt++) {
        service = create(invalid);
        advance(service.nextAlarm()!);
        expect(await service.runDue()).toMatchObject({ status: 'failed' });
      }
      expect(invalid).toHaveBeenCalledTimes(2);
      expect(create().nextAlarm()).toBeNull();
      expect(store.get('alice', { path: 'USER.md' }).deleted).toBe(true);
      expect(JSON.stringify(storage.sql.exec('SELECT * FROM markdown_consolidation_receipts').toArray())).not.toContain('hostile');
      expect(store.readFile('alice', 'DREAMS.md')).not.toContain('FIXTURE_SECRET');
    });
  });

  it.each(['paraphrase', 'wrong-owner', 'out-of-range', 'overlap', 'repeated-evidence', 'unknown-field', 'bad-target'])('rejects %s candidates against supplied exact spans', async kind => {
    await fixture(async ({ create, change, advance, store }) => {
      const service = create(async request => {
        const candidate = selected(request);
        if (kind === 'paraphrase') candidate.quote = 'The user likes brief prose.';
        if (kind === 'wrong-owner') candidate.sources[0]!.path = 'memory/2026-09-21.md';
        if (kind === 'out-of-range') candidate.sources[0]!.to_line = 65;
        if (kind === 'overlap') { candidate.sources.push(candidate.sources[0]!); candidate.quote += '\n' + candidate.quote; }
        if (kind === 'repeated-evidence') {
          candidate.sources.push({ ...candidate.sources[0]!, from_line: 2, to_line: 2 });
          candidate.quote += '\n' + candidate.quote;
        }
        if (kind === 'bad-target') candidate.target = 'DREAMS.md';
        return { candidates: [{ ...candidate, ...(kind === 'unknown-field' ? { tool_calls: [] } : {}) }] };
      });
      change(service, PATH, 'I prefer concise answers.\nI prefer concise answers.');
      change(service, 'memory/2026-09-21.md', 'I prefer concise answers.', 'bob');
      advance(START + DAY);
      expect(await service.runDue()).toMatchObject({ status: 'failed', reason: 'retry_scheduled' });
      expect(store.get('alice', { path: 'USER.md' }).deleted).toBe(true);
    });
  });

  it('guards source and curated secrets before inference', async () => {
    await fixture(async ({ create, change, advance, storage }) => {
      const complete = vi.fn<MarkdownMemoryCompletion>(async () => ({ candidates: [] }));
      const service = create(complete);
      change(service, 'USER.md', 'FIXTURE_SECRET curated');
      change(service, PATH, 'FIXTURE_SECRET source');
      advance(START + DAY);
      expect(await service.runDue()).toMatchObject({ status: 'failed', reason: 'secret_guard' });
      expect(complete).not.toHaveBeenCalled();
      expect(JSON.stringify(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray())).not.toContain('FIXTURE_SECRET');
    });
  });

  it('claims only once until lease expiry and fences the late original worker', async () => {
    await fixture(async ({ create, change, advance, store }) => {
      const entered = deferred<MarkdownMemoryCompletionRequest>();
      const reply = deferred<unknown>();
      const service = create(async request => { entered.resolve(request); return reply.promise; });
      change(service, PATH, 'One authoritative preference.');
      advance(START + DAY);
      const running = service.runDue();
      const request = await entered.promise;
      expect(await create().runDue()).toBeNull();
      advance(service.nextAlarm()!);
      expect(await create().runDue()).toMatchObject({ status: 'committed' });
      reply.resolve({ candidates: [selected(request)] });
      expect(await running).toMatchObject({ status: 'stale', reason: 'claim_replaced' });
      expect(store.readFile('alice', 'USER.md').match(/> One authoritative preference\./g)).toHaveLength(1);
    });
  });

  it('bounds source work by lines, bytes, and documents, continuing durably on the following day', async () => {
    await fixture(async ({ create, change, advance }) => {
      const requests: MarkdownMemoryCompletionRequest[] = [];
      const complete: MarkdownMemoryCompletion = async request => { requests.push(request); return { candidates: [] }; };
      const service = create(complete);
      change(service, PATH, Array.from({ length: 100 }, (_, i) => `Line ${i}.`).join('\n'));
      for (let i = 0; i < 10; i++) change(service, `memory/2026-09-23-topic-${i}.md`, 'é'.repeat(4000));
      advance(START + DAY);
      expect(await service.runDue()).toMatchObject({ status: 'empty' });
      const sources = (requests[0]!.input as Input).sources;
      expect(sources.length).toBeLessThanOrEqual(8);
      expect(sources[0]).toMatchObject({ from_line: 1, to_line: 64 });
      expect(sources.reduce((n, source) => n + new TextEncoder().encode(source.content).length, 0)).toBeLessThanOrEqual(12_000);
      expect(service.status('alice').has_more).toBe(true);
      const restored = create(complete);
      expect(restored.status('alice').pending[0]).toMatchObject({ next_line: 65 });
      advance(restored.nextAlarm()!);
      await restored.runDue();
      expect((requests[1]!.input as Input).sources[0]).toMatchObject({ from_line: 65, to_line: 100 });
    });
  });

  it('rolls back the canonical source delete and derived cleanup together when invalidation fails', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const service = create();
      change(service, PATH, 'Transactional preference.');
      advance(START + DAY);
      await service.runDue();
      const guarded = new MarkdownMemoryConsolidation(storage, store, {
        complete: async () => ({ candidates: [] }), containsSecret: () => false,
      });
      const original = store.write.bind(store);
      const mock = vi.spyOn(store, 'write').mockImplementation((owner, input) => {
        if ((input as { path: string }).path === 'USER.md') throw new Error('fixture index failure');
        return original(owner, input);
      });
      expect(() => change(guarded, PATH, null)).toThrow('fixture index failure');
      mock.mockRestore();
      expect(store.readFile('alice', PATH)).toBe('Transactional preference.');
      expect(store.readFile('alice', 'USER.md')).toContain('Transactional preference.');
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_entries').toArray()).toHaveLength(1);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray().length).toBeGreaterThan(0);
    });
  });

  it('rolls back promoted bodies, provenance, and preimages if the audit write fails', async () => {
    await fixture(async ({ change, store, storage }) => {
      let now = START;
      const service = new MarkdownMemoryConsolidation(storage, store, {
        complete: async request => ({ candidates: [selected(request)] }), containsSecret: () => false, now: () => now,
      });
      change(service, PATH, 'All or nothing preference.');
      const original = store.write.bind(store);
      const mock = vi.spyOn(store, 'write').mockImplementation((owner, input) => {
        if ((input as { path: string }).path === 'DREAMS.md') throw new Error('fixture audit failure');
        return original(owner, input);
      });
      now += DAY;
      expect(await service.runDue()).toMatchObject({ status: 'failed', reason: 'retry_scheduled' });
      mock.mockRestore();
      expect(store.get('alice', { path: 'USER.md' }).deleted).toBe(true);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_entries').toArray()).toEqual([]);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray()).toEqual([]);
      expect(service.status('alice').pending).toHaveLength(1);
    });
  });

  it('retains bounded receipts and audit preimages while preserving manual DREAMS text', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      const complete: MarkdownMemoryCompletion = async () => ({ candidates: [] });
      let service = create(complete);
      change(service, 'DREAMS.md', 'Manual audit notes.');
      for (let day = 0; day < 35; day++) {
        const date = new Date(START + day * DAY).toISOString().slice(0, 10);
        change(service, `memory/${date}.md`, `Transient source ${day}.`);
        advance(service.nextAlarm()!);
        expect(await service.runDue()).toMatchObject({ status: 'empty' });
        service = create(complete);
      }
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_receipts').toArray()).toHaveLength(32);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray()).toHaveLength(32);
      expect(service.status('alice').receipts).toHaveLength(20);
      const report = store.readFile('alice', 'DREAMS.md');
      expect(report).toContain('Manual audit notes.');
      expect(report.match(/<!-- consolidation-report -->/g)).toHaveLength(1);
      expect(report.length).toBeLessThan(8192);
      expect(service.nextAlarm()).toBeNull();
    });
  });

  it('supports attributed supersession and clears historical preimages when the superseded source is deleted', async () => {
    await fixture(async ({ create, change, advance, store, storage }) => {
      let service = create();
      change(service, PATH, 'I prefer short answers.');
      advance(START + DAY);
      await service.runDue();
      service = create(async request => ({ candidates: [{ ...selected(request), replace_ids: [(request.input as Input).managed_entries[0]!.id] }] }));
      change(service, 'memory/2026-09-23.md', 'I now prefer detailed answers.');
      advance(START + 2 * DAY);
      expect(await service.runDue()).toMatchObject({ status: 'committed' });
      expect(store.readFile('alice', 'USER.md')).not.toContain('I prefer short answers.');
      expect(JSON.stringify(storage.sql.exec('SELECT content FROM markdown_consolidation_preimages').toArray())).toContain('I prefer short answers.');
      change(service, PATH, null);
      expect(storage.sql.exec('SELECT * FROM markdown_consolidation_preimages').toArray()).toEqual([]);
      expect(store.readFile('alice', 'USER.md')).toContain('I now prefer detailed answers.');
    });
  });
});
