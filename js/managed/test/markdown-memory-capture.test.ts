import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { DurableAgentSession } from '../src/index';
import { MarkdownMemoryConsolidation } from '../src/markdown-memory-consolidation';
import { MarkdownMemoryCapture } from '../src/markdown-memory-capture';
import { MarkdownMemoryError, MarkdownMemoryStore } from '../src/markdown-memory';
import { MarkdownMemoryFlush, type MarkdownMemoryFlushInput } from '../src/markdown-memory-flush';
import type { MarkdownMemoryCompletion } from '../src/markdown-memory-ai';

const ALICE = 'personal:fixture-alice';
const BOB = 'personal:fixture-bob';
const input = (boundary = 'source-1', text = 'I prefer concise progress updates.'): MarkdownMemoryFlushInput => ({
  boundary_id: boundary, session_id: 'fixture-session', messages: [{ id: boundary, role: 'user', text }],
});
const select: MarkdownMemoryCompletion = async request => ({ spans: (request.input as MarkdownMemoryFlushInput).messages
  .map(message => ({ message_id: message.id, quote: message.text })) });
async function fixture(run: (value: {
  storage: DurableObjectStorage; store: MarkdownMemoryStore;
  create: (complete?: MarkdownMemoryCompletion, limit?: number) => MarkdownMemoryCapture;
}) => Promise<void>) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
    const storage = state.storage;
    await run({ storage, store: new MarkdownMemoryStore(storage),
      create: (complete = select, dailyInferenceLimit) => new MarkdownMemoryCapture(storage,
        new MarkdownMemoryFlush(storage, complete, { dailyInferenceLimit })) });
  });
}

describe('per-message capture in the memory service', () => {
  it('starts immediately and stores exact private evidence without scheduling an alarm', async () => {
    await fixture(async ({ storage, store, create }) => {
      const complete = vi.fn(select);
      const capture = create(complete);
      const result = capture.capture(ALICE, input());
      await expect(result).resolves.toMatchObject({ selected_spans: 1 });
      expect(complete).toHaveBeenCalledOnce();
      expect(store.list(ALICE)).toHaveLength(1);
      expect(store.list(BOB)).toEqual([]);
      expect(capture.status(ALICE)).toMatchObject({ mode: 'on_message', active: 0, attempts: 1 });
      expect(await storage.getAlarm()).toBeNull();
      expect(storage.sql.exec("SELECT name FROM sqlite_master WHERE name='markdown_memory_capture'").toArray()).toEqual([]);
    });
  });

  it('handles a later event while an earlier decision is still running', async () => {
    await fixture(async ({ create, store }) => {
      let release!: (value: unknown) => void;
      const held = new Promise(resolve => { release = resolve; });
      const complete = vi.fn<MarkdownMemoryCompletion>().mockReturnValueOnce(held).mockImplementation(select);
      const capture = create(complete);
      const first = capture.capture(ALICE, input());
      await expect(capture.capture(ALICE, input('second'))).resolves.toMatchObject({ selected_spans: 1 });
      expect(store.list(ALICE)).toHaveLength(1);
      release({ spans: [{ message_id: 'source-1', quote: input().messages[0]!.text }] });
      await first;
      expect(store.list(ALICE)).toHaveLength(2);
    });
  });

  it('uses existing receipts for repeated delivery and rejects changed source text', async () => {
    await fixture(async ({ create, store }) => {
      const complete = vi.fn(select);
      const capture = create(complete);
      const first = capture.capture(ALICE, input());
      await expect(capture.capture(ALICE, input())).resolves.toEqual(await first);
      await expect(create(complete).capture(ALICE, input())).resolves.toEqual(await first);
      await expect(capture.capture(ALICE, input('source-1', 'I prefer long updates.')))
        .rejects.toMatchObject({ code: 'memory_boundary_conflict' });
      expect(complete).toHaveBeenCalledOnce();
      expect(store.list(ALICE)).toHaveLength(1);
    });
  });

  it('cancels an active decision and rejects late delivery after withdrawal', async () => {
    await fixture(async ({ create, store }) => {
      let release!: (value: unknown) => void;
      const complete = vi.fn<MarkdownMemoryCompletion>(() => new Promise(resolve => { release = resolve; }));
      const capture = create(complete);
      const task = capture.capture(ALICE, input());
      const rejected = expect(task).rejects.toMatchObject({ code: 'memory_inference_cancelled' });
      await expect.poll(() => complete.mock.calls.length).toBe(1);
      capture.cancel(ALICE, 'source-1');
      release({ spans: [{ message_id: 'source-1', quote: input().messages[0]!.text }] });
      await rejected;
      expect(store.list(ALICE)).toEqual([]);
      capture.cancel(ALICE, 'late-delivery');
      await expect(create(complete).capture(ALICE, input('late-delivery'))).resolves.toBeUndefined();
      expect(complete).toHaveBeenCalledOnce();
    });
  });

  it('retracts completed capture on withdrawal, preserves receipts and informs consolidation', async () => {
    await fixture(async ({ storage, store }) => {
      const onRetract = vi.fn();
      const capture = new MarkdownMemoryCapture(storage, new MarkdownMemoryFlush(storage, select), onRetract);
      const receipt = (await capture.capture(ALICE, input()))!;
      expect(store.list(ALICE)).toEqual([receipt.path]);
      capture.cancel(ALICE, 'source-1');
      expect(store.list(ALICE)).toEqual([]);
      expect(onRetract).toHaveBeenCalledWith(ALICE, receipt.path, 2);
      await expect(capture.capture(ALICE, input())).resolves.toBeUndefined();
      expect(capture.status(ALICE).receipts).toHaveLength(1);
    });
  });

  it('retracts a withdrawn capture from periodic consolidated memory too', async () => {
    await fixture(async ({ storage, store }) => {
      let now = Date.now();
      const consolidation = new MarkdownMemoryConsolidation(storage, store, {
        now: () => now, containsSecret: () => false,
        complete: async request => {
          const source = (request.input as { sources: { path: string; revision: number; from_line: number; content: string }[] }).sources[0]!;
          const lines = source.content.split('\n');
          const index = lines.findIndex(line => line === `> ${input().messages[0]!.text}`);
          expect(index).toBeGreaterThanOrEqual(0);
          return { candidates: [{ target: 'USER.md', quote: lines[index], replace_ids: [], sources: [{
            path: source.path, revision: source.revision, from_line: source.from_line + index, to_line: source.from_line + index,
          }] }] };
        },
      });
      const changed = (owner: string, path: string, revision: number) => consolidation.noteChange(owner, path, revision, 'capture');
      const capture = new MarkdownMemoryCapture(storage, new MarkdownMemoryFlush(storage, select, { onPersist: changed }), changed);
      const receipt = (await capture.capture(ALICE, input()))!;
      expect(store.list(ALICE)).toEqual([receipt.path]);
      now += 86_400_000;
      await expect(consolidation.runDue()).resolves.toMatchObject({ status: 'committed', additions: 1 });
      expect(store.readFile(ALICE, 'USER.md')).toContain(input().messages[0]!.text);
      capture.cancel(ALICE, 'source-1');
      expect(store.get(ALICE, { path: receipt.path }).deleted).toBe(true);
      expect(store.readFile(ALICE, 'USER.md')).not.toContain(input().messages[0]!.text);
      expect(store.search(ALICE, { query: 'concise' }).results).toEqual([]);
    });
  });

  it('preserves explicit curation of a generated note during withdrawal', async () => {
    await fixture(async ({ create, store }) => {
      const capture = create();
      const receipt = (await capture.capture(ALICE, input()))!;
      store.write(ALICE, { operation: 'put', path: receipt.path, content: 'Manually curated context.' });
      capture.cancel(ALICE, 'source-1');
      expect(store.readFile(ALICE, receipt.path!)).toBe('Manually curated context.');
    });
  });

  it('prevents in-flight capture from undoing manual curation', async () => {
    await fixture(async ({ create, store }) => {
      let release!: (value: unknown) => void;
      const capture = create(() => new Promise(resolve => { release = resolve; }));
      const task = capture.capture(ALICE, input());
      const rejected = expect(task).rejects.toMatchObject({ code: 'memory_inference_cancelled' });
      await expect.poll(() => Boolean(release)).toBe(true);
      capture.cancelActive(ALICE);
      release({ spans: [{ message_id: 'source-1', quote: input().messages[0]!.text }] });
      await rejected;
      expect(store.list(ALICE)).toEqual([]);
    });
  });

  it('drops failed or budget-limited decisions without retrying or creating a backlog', async () => {
    await fixture(async ({ create, storage, store }) => {
      const complete = vi.fn<MarkdownMemoryCompletion>().mockRejectedValue(new Error('fixture provider failure'));
      const capture = create(complete, 1);
      await expect(capture.capture(ALICE, input())).rejects.toThrow('fixture provider failure');
      await expect(capture.capture(ALICE, input('second'))).rejects.toMatchObject({ code: 'memory_inference_budget' });
      expect(capture.status(ALICE)).toMatchObject({ active: 0, remaining: 0 });
      expect(complete).toHaveBeenCalledOnce();
      expect(store.list(ALICE)).toEqual([]);
      expect(await storage.getAlarm()).toBeNull();
    });
  });

  it('bounds concurrent decisions without queuing raw sources', async () => {
    await fixture(async ({ create, store }) => {
      const capture = create(async () => new Promise(() => {}));
      const tasks = Array.from({ length: 8 }, (_, i) => capture.capture(ALICE, input(`event-${i}`)).catch(() => {}));
      expect(() => capture.capture(ALICE, input('overflow'))).toThrowError('memory capture is busy');
      capture.cancelActive(ALICE);
      await Promise.all(tasks);
      expect(capture.status(ALICE).active).toBe(0);
      expect(store.list(ALICE)).toEqual([]);
    });
  });

  it('rejects team, malformed, oversized and assistant sources before saving', async () => {
    await fixture(async ({ create, store }) => {
      const complete = vi.fn(select), capture = create(complete);
      expect(() => capture.capture('fixture-team', input())).toThrowError(MarkdownMemoryError);
      expect(() => capture.capture(ALICE, { ...input(), messages: [{ id: 'assistant', role: 'assistant', text: 'Invented preference.' }] }))
        .toThrowError(MarkdownMemoryError);
      await expect(capture.capture(ALICE, input('large', '🟢'.repeat(20_000)))).rejects.toMatchObject({ code: 'invalid_memory_flush' });
      expect(complete).not.toHaveBeenCalled();
      expect(store.list(ALICE)).toEqual([]);
    });
  });
});
