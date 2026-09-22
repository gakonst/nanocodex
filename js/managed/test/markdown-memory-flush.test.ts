import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { MemoryFlushFixture } from './markdown-memory-flush-worker';
import { MarkdownMemoryStore } from '../src/markdown-memory';
import { MarkdownMemoryFlush, type MarkdownMemoryFlushInput, type MarkdownMemoryFlushMessage } from '../src/markdown-memory-flush';
import type { MarkdownMemoryCompletion } from '../src/markdown-memory-ai';

const NOW = Date.parse('2026-09-22T10:00:00Z');
const user = (id: string, text: string): MarkdownMemoryFlushMessage => ({ id, role: 'user', text });
const source = user('u1', 'I prefer concise status updates.');
const input = (boundary = 'b1', messages = [source]): MarkdownMemoryFlushInput => ({ boundary_id: boundary, session_id: 'session-1', messages });
const span = (message: MarkdownMemoryFlushMessage, start = 0, end = message.text.length) => ({ message_id: message.id, start, end, quote: message.text.slice(start, end) });
const select: MarkdownMemoryCompletion = async request => ({ spans: (request.input as MarkdownMemoryFlushInput).messages.map(message => span(message)) });
const options = { now: () => NOW, containsSecret: (text: string) => text.includes('synthetic-vault-value') };
async function withStorage(run: (storage: DurableObjectStorage, store: MarkdownMemoryStore) => Promise<void>) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<MemoryFlushFixture> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => run(state.storage, new MarkdownMemoryStore(state.storage)));
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

describe('precompaction memory flush on Durable Object SQLite', () => {
  it('commits exact evidence, truncation metadata and the consolidation event atomically, then replays after reconstruction', async () => {
    await withStorage(async (storage, store) => {
      storage.sql.exec('CREATE TABLE fixture_events(owner TEXT,path TEXT,revision INTEGER)');
      const complete = vi.fn(select);
      const onPersist = vi.fn((owner: string, path: string, revision: number) => { storage.sql.exec('INSERT INTO fixture_events VALUES(?,?,?)', owner, path, revision); });
      const request = { ...input(), truncated: true };
      const receipt = await new MarkdownMemoryFlush(storage, complete, { ...options, onPersist }).flush('alice', request);
      expect(receipt).toMatchObject({ boundary_id: 'b1', request_hash: expect.stringMatching(/^[a-f0-9]{64}$/), revision: 1, selected_spans: 1, truncated: true, created_at: NOW });
      expect(store.readFile('alice', receipt.path!)).toContain(`> ${source.text}`);
      expect(store.readFile('alice', receipt.path!)).toContain('Truncated context: true');
      expect(storage.sql.exec('SELECT * FROM fixture_events').toArray()).toEqual([{ owner: 'alice', path: receipt.path, revision: 1 }]);
      expect(await new MarkdownMemoryFlush(storage, complete, { ...options, onPersist }).flush('alice', request)).toEqual(receipt);
      expect(complete).toHaveBeenCalledTimes(1);
      expect(onPersist).toHaveBeenCalledTimes(1);
      await expect(new MarkdownMemoryFlush(storage, complete, options).flush('alice', { ...request, truncated: false })).rejects.toMatchObject({ code: 'memory_boundary_conflict' });
      expect(new MarkdownMemoryFlush(storage, select, options).status('alice')).toMatchObject({ attempts: 1, remaining: 47, pending: 0, receipts: [receipt] });
      expect(new MarkdownMemoryFlush(storage, select, options).status('bob')).toMatchObject({ attempts: 0, receipts: [] });
      expect(JSON.stringify(storage.sql.exec('SELECT * FROM markdown_memory_flushes').toArray())).not.toContain(source.text);
    });
  });

  it('excludes assistant, secret, recalled and quoted text before inference and accepts an empty durable selection', async () => {
    await withStorage(async (storage, store) => {
      const messages = [source, { ...user('a', 'I prefer fabricated assistant facts.'), role: 'assistant' as const },
        user('s1', 'My password: fixture-only'), user('s2', 'My code is synthetic-vault-value'),
        user('s3', '{"api_key":"fixture-only"}'), user('r1', '<memory_context>I prefer invented facts.</memory_context>'),
        user('r2', '> I prefer quoted claims.'), user('r3', '[recalled memory] I prefer old facts.'), user('r4', '"I prefer a quoted claim."')];
      const complete = vi.fn(async request => {
        expect((request.input as MarkdownMemoryFlushInput).messages).toEqual([source]);
        return { spans: [] };
      });
      const receipt = await new MarkdownMemoryFlush(storage, complete, options).flush('alice', input('b1', messages));
      expect(receipt).toMatchObject({ selected_spans: 0, path: null, revision: null, truncated: false });
      expect(store.list('alice')).toEqual([]);
      const emptyComplete = vi.fn(select);
      const flush = new MarkdownMemoryFlush(storage, emptyComplete, options);
      await flush.flush('alice', input('b2', messages.slice(1)));
      expect(emptyComplete).not.toHaveBeenCalled();
      expect(flush.status('alice').attempts).toBe(1);
    });
  });

  it('rejects fabricated, assistant, secret and recalled evidence even when a completion selects it', async () => {
    await withStorage(async (storage, store) => {
      const messages = [source, { ...user('a', 'I prefer an assistant invention.'), role: 'assistant' as const }, user('s', 'My code is synthetic-vault-value'), user('r', '<recalled>I prefer old facts.</recalled>')];
      for (const [index, selected] of [span(messages[1]!), span(messages[2]!), span(messages[3]!), { ...span(source), quote: 'I prefer long status updates.' }, { ...span(source), message_id: 'missing' }].entries()) {
        await expect(new MarkdownMemoryFlush(storage, async () => ({ spans: [selected] }), options).flush('alice', input(`bad-${index}`, messages)))
          .rejects.toMatchObject({ code: 'memory_inference_invalid' });
      }
      expect(store.list('alice')).toEqual([]);
    });
  });

  it('preserves complete negation and correction context, including corrections within one message', async () => {
    await withStorage(async (storage, store) => {
      const negative = user('n', 'I do not want notifications.');
      const multiline = user('m', 'I prefer notifications.\nActually, I no longer want notifications.');
      const later = user('c', 'Actually, I prefer weekly summaries instead.');
      const examples = [
        { messages: [negative], spans: [span(negative, 9)] },
        { messages: [multiline], spans: [span(multiline, 0, multiline.text.indexOf('\n'))] },
        { messages: [source, later], spans: [span(source)] },
      ];
      for (const [i, example] of examples.entries()) {
        await expect(new MarkdownMemoryFlush(storage, async () => ({ spans: example.spans }), options).flush('alice', input(`bad-${i}`, example.messages)))
          .rejects.toMatchObject({ code: 'memory_inference_invalid' });
      }
      const receipt = await new MarkdownMemoryFlush(storage, async () => ({ spans: [span(later), span(negative)] }), options).flush('alice', input('good', [negative, later]));
      const content = store.readFile('alice', receipt.path!);
      expect(content.indexOf(negative.text)).toBeLessThan(content.indexOf(later.text));
      expect(content).toContain(`> ${negative.text}`);
      expect(content).toContain(`> ${later.text}`);
    });
  });

  it('merges overlapping selections and durably deduplicates evidence across boundaries and deletion', async () => {
    await withStorage(async (storage, store) => {
      const message = user('multi', 'I prefer concise updates.\nMy project uses copper fixtures.');
      const complete = vi.fn(async () => ({ spans: [span(message), span(message, 0, message.text.indexOf('\n')), span(message)] }));
      const flush = new MarkdownMemoryFlush(storage, complete, options);
      const first = await flush.flush('alice', input('first', [message]));
      expect(first.selected_spans).toBe(1);
      store.write('alice', { operation: 'delete', path: first.path, expected_revision: 1 });
      expect(await new MarkdownMemoryFlush(storage, complete, options).flush('alice', input('first', [message]))).toEqual(first);
      expect(complete).toHaveBeenCalledTimes(1);
      const second = await new MarkdownMemoryFlush(storage, complete, options).flush('alice', input('second', [message]));
      expect(second).toMatchObject({ path: null, selected_spans: 0 });
      expect(store.list('alice')).toEqual([]);
      expect(store.get('alice', { path: first.path }).revision).toBe(2);
      const evidence = storage.sql.exec('SELECT * FROM markdown_memory_flush_evidence').toArray();
      expect(evidence).toHaveLength(1);
      expect(JSON.stringify(evidence)).not.toContain(message.text);
      await expect(new MarkdownMemoryFlush(storage, select, options).flush('alice', input('changed', [{ ...message, text: 'I prefer rewritten history.' }]))).rejects.toMatchObject({ code: 'memory_source_conflict' });
      // A different authenticated owner has an independent evidence namespace.
      expect((await flush.flush('bob', input('first', [message]))).selected_spans).toBe(1);
    });
  });

  it('deduplicates two overlapping inferences that complete in reverse order', async () => {
    await withStorage(async (storage, store) => {
      const waiting = deferred<unknown>();
      const started = deferred<void>();
      const first = new MarkdownMemoryFlush(storage, async () => { started.resolve(); return waiting.promise; }, options).flush('alice', input('first'));
      await started.promise;
      const second = await new MarkdownMemoryFlush(storage, select, options).flush('alice', input('second'));
      waiting.resolve({ spans: [span(source)] });
      expect(await first).toMatchObject({ path: null, selected_spans: 0 });
      expect(second.selected_spans).toBe(1);
      expect(store.list('alice')).toEqual([second.path]);
    });
  });

  it('rolls back note, search index, dedupe rows, callback SQL and receipt together when the callback fails', async () => {
    await withStorage(async (storage, store) => {
      storage.sql.exec('CREATE TABLE fixture_events(owner TEXT,path TEXT,revision INTEGER)');
      const flush = new MarkdownMemoryFlush(storage, select, { ...options, onPersist: (owner, path, revision) => {
        storage.sql.exec('INSERT INTO fixture_events VALUES(?,?,?)', owner, path, revision);
        throw new Error('fixture event failure');
      } });
      await expect(flush.flush('alice', input())).rejects.toThrow('fixture event failure');
      expect(store.list('alice')).toEqual([]);
      expect(store.search('alice', { query: 'concise' }).results).toEqual([]);
      expect(storage.sql.exec('SELECT * FROM fixture_events').toArray()).toEqual([]);
      expect(storage.sql.exec('SELECT * FROM markdown_memory_flush_evidence').toArray()).toEqual([]);
      expect(flush.status('alice')).toMatchObject({ attempts: 1, pending: 0, receipts: [] });
      expect((await new MarkdownMemoryFlush(storage, select, options).flush('alice', input())).selected_spans).toBe(1);
    });
  });

  it('coalesces same-instance requests, rejects active durable leases after reconstruction and rejects mismatched replay', async () => {
    await withStorage(async (storage) => {
      const waiting = deferred<unknown>();
      const started = deferred<void>();
      const complete = vi.fn(async () => { started.resolve(); return waiting.promise; });
      const flush = new MarkdownMemoryFlush(storage, complete, options);
      const first = flush.flush('alice', input());
      await started.promise;
      const duplicate = flush.flush('alice', input());
      expect(flush.status('alice')).toMatchObject({ attempts: 1, pending: 1 });
      await expect(new MarkdownMemoryFlush(storage, complete, options).flush('alice', input())).rejects.toMatchObject({ code: 'memory_flush_pending' });
      await expect(flush.flush('alice', input('b1', [user('u2', 'I prefer something else.')]))).rejects.toMatchObject({ code: 'memory_boundary_conflict' });
      waiting.resolve({ spans: [span(source)] });
      expect(await duplicate).toEqual(await first);
      expect(complete).toHaveBeenCalledTimes(1);
    });
  });

  it('allows lease recovery and prevents a late old completion from writing', async () => {
    await withStorage(async (storage, store) => {
      let now = NOW;
      const waiting = deferred<unknown>();
      const started = deferred<void>();
      const timed = { ...options, now: () => now };
      const first = new MarkdownMemoryFlush(storage, async () => { started.resolve(); return waiting.promise; }, timed).flush('alice', input());
      const oldResult = expect(first).rejects.toMatchObject({ code: 'memory_flush_pending' });
      await started.promise;
      now += 31_001;
      const second = await new MarkdownMemoryFlush(storage, select, timed).flush('alice', input());
      waiting.resolve({ spans: [span(source)] });
      await oldResult;
      expect(store.list('alice')).toEqual([second.path]);
      expect(new MarkdownMemoryFlush(storage, select, timed).status('alice')).toMatchObject({ attempts: 2, pending: 0, receipts: [second] });
    });
  });

  it('persists failed inference budget across reconstruction and resets by UTC day', async () => {
    await withStorage(async storage => {
      const complete = vi.fn(async () => { throw new Error('fixture completion failed'); });
      const limited = { ...options, dailyInferenceLimit: 1 };
      await expect(new MarkdownMemoryFlush(storage, complete, limited).flush('alice', input())).rejects.toThrow('fixture completion failed');
      await expect(new MarkdownMemoryFlush(storage, complete, limited).flush('alice', input())).rejects.toMatchObject({ code: 'memory_inference_budget', status: 429 });
      expect(complete).toHaveBeenCalledTimes(1);
      expect(new MarkdownMemoryFlush(storage, select, limited).status('alice')).toMatchObject({ attempts: 1, remaining: 0 });
      expect((await new MarkdownMemoryFlush(storage, select, { ...limited, now: () => NOW + 86_400_000 }).flush('alice', input())).selected_spans).toBe(1);
    });
  });

  it('cancels and times out inference without allowing a late result to persist', async () => {
    await withStorage(async (storage, store) => {
      const controller = new AbortController();
      controller.abort();
      const never = vi.fn(async () => new Promise<unknown>(() => {}));
      const flush = new MarkdownMemoryFlush(storage, never, { ...options, timeoutMs: 5 });
      await expect(flush.flush('alice', input(), controller.signal)).rejects.toMatchObject({ code: 'memory_inference_cancelled' });
      expect(never).not.toHaveBeenCalled();
      expect(flush.status('alice').attempts).toBe(0);
      await expect(flush.flush('alice', input())).rejects.toMatchObject({ code: 'memory_inference_timeout' });
      const waiting = deferred<unknown>();
      const started = deferred<void>();
      const activeController = new AbortController();
      const active = new MarkdownMemoryFlush(storage, async () => { started.resolve(); return waiting.promise; }, options).flush('alice', input('active'), activeController.signal);
      const cancelled = expect(active).rejects.toMatchObject({ code: 'memory_inference_cancelled' });
      await started.promise;
      activeController.abort();
      await cancelled;
      waiting.resolve({ spans: [span(source)] });
      await Promise.resolve();
      expect(store.list('alice')).toEqual([]);
      expect(flush.status('alice')).toMatchObject({ attempts: 2, pending: 0, receipts: [] });
    });
  });

  it('rejects invalid input before spending and bounds status receipt history', async () => {
    await withStorage(async storage => {
      const complete = vi.fn(select);
      const flush = new MarkdownMemoryFlush(storage, complete, options);
      for (const request of [null, { ...input(), extra: true }, { ...input(), truncated: 'yes' }, input('bad/id'), input('dup', [source, source]), input('nul', [user('u', '\0')]), input('long', [user('u', 'é'.repeat(33_000))]), input('role', [{ ...source, role: 'tool' as 'user' }])]) {
        await expect(flush.flush('alice', request)).rejects.toThrow();
      }
      expect(complete).not.toHaveBeenCalled();
      for (let i = 0; i < 23; i++) await flush.flush('alice', input(`empty-${i}`, []));
      expect(flush.status('alice')).toMatchObject({ attempts: 0, has_more: true });
      expect(flush.status('alice').receipts).toHaveLength(20);
      expect(() => flush.status('')).toThrow();
    });
  });
});
