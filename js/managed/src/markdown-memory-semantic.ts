import { createHash } from 'node:crypto';
import { MarkdownMemoryStore } from './markdown-memory';

type Item = { chunk_id: number; owner: string; path: string; revision: number; operation: string;
  item_id: string | null; attempts: number; retry_at: number | null };
type Hit = { path: string; revision: number; from_line: number; to_line: number; snippet: string };
type Candidate = Hit & { chunk_id: number; score: number };
const KIND = 'markdown_memory';
const MAX_CANDIDATES = 40;
const TIMEOUT_MS = 2500;

async function disposeResult<T extends object, R>(promise: Promise<T>, read: (value: T) => R): Promise<R> {
  const value = await promise;
  try { return read(value); } finally { (value as T & Partial<Disposable>)[Symbol.dispose]?.(); }
}
async function itemsOperation<T>(binding: AiSearchInstance, run: (items: AiSearchItems) => Promise<T>): Promise<T> {
  const items = binding.items;
  try { return await run(items); } finally { (items as AiSearchItems & Partial<Disposable>)[Symbol.dispose]?.(); }
}
async function bounded<T>(promise: Promise<T>, ms = TIMEOUT_MS): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([promise, new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error('markdown semantic timeout')), ms);
  })]); } finally { if (timer !== undefined) clearTimeout(timer); }
}
const keyFor = (organization: string, row: Item) => 'markdown-' + createHash('sha256')
  .update(JSON.stringify([organization, row.owner, row.path, row.revision, row.chunk_id])).digest('hex') + '.md';
const missing = (error: unknown) => typeof error === 'object' && error !== null
  && ('status' in error && error.status === 404 || 'statusCode' in error && error.statusCode === 404);

/** One shared instance per DO. Writes enqueue in the canonical SQL transaction.
 * The parent alarm calls drain() and includes nextRetryAt() in its minimum deadline.
 * Missing bindings retain the queue for revival after configuration/restart.
 */
export class MarkdownMemorySemantic {
  private readonly store: MarkdownMemoryStore;
  private draining?: Promise<void>;
  private readonly active = new Set<number>();
  constructor(private readonly storage: DurableObjectStorage, private readonly organizationId: string,
    private readonly binding?: AiSearchInstance) {
    this.store = new MarkdownMemoryStore(storage);
  }
  nextRetryAt(): number | undefined {
    if (!this.binding) return undefined;
    if (this.active.size >= 4) return Date.now() + 5000;
    const active = [...this.active];
    const retry = this.storage.sql.exec<{ retry_at: number }>(
      `SELECT retry_at FROM markdown_memory_ai_items WHERE retry_at IS NOT NULL
        AND chunk_id NOT IN (${active.length ? active.map(() => '?').join(',') : 'SELECT NULL WHERE 0'})
        ORDER BY retry_at LIMIT 1`, ...active,
    ).toArray()[0]?.retry_at;
    return active.length ? Math.min(retry ?? Infinity, Date.now() + 5000) : retry;
  }
  drain(now = Date.now()): Promise<void> {
    if (!this.binding) return Promise.resolve();
    if (this.draining) return this.draining;
    const task = this.drainBatch(now).finally(() => { if (this.draining === task) this.draining = undefined; });
    this.draining = task;
    return task;
  }
  private async drainBatch(now: number): Promise<void> {
    // A timed-out RPC can finish later. Keep it in the concurrency budget until
    // settlement, avoiding overlapping writes and unbounded orphan RPCs.
    if (this.active.size >= 4) return;
    const active = [...this.active];
    const rows = this.storage.sql.exec<Item>(`SELECT * FROM markdown_memory_ai_items
      WHERE retry_at IS NOT NULL AND retry_at<=?
      AND chunk_id NOT IN (${active.length ? active.map(() => '?').join(',') : 'SELECT NULL WHERE 0'})
      ORDER BY retry_at,chunk_id LIMIT ?`, now, ...active, 4 - active.length).toArray();
    await Promise.all(rows.map(async row => {
      this.active.add(row.chunk_id);
      const project = this.project(row).finally(() => this.active.delete(row.chunk_id));
      try { await bounded(project, 5000); } catch {
        // Do not log text or scope identifiers. The durable row is the retry receipt.
      } finally {
        this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET attempts=attempts+1,retry_at=?
          WHERE chunk_id=? AND retry_at IS NOT NULL AND operation=?`,
        now + Math.min(300_000, 1000 * 2 ** Math.min(row.attempts, 8)), row.chunk_id, row.operation);
      }
    }));
  }
  private async project(row: Item): Promise<void> {
    const key = keyFor(this.organizationId, row);
    await itemsOperation(this.binding!, async items => {
      // Exact-key reconciliation precedes every upload, including a retry after
      // an ambiguous timeout. Built-in keys are unique, so retries are idempotent.
      const listed = await disposeResult(items.list({ key, source: 'builtin', per_page: 50 } as
        AiSearchListItemsParams & { key: string }), result => result.result.filter(item => item.key === key)
          .map(item => ({ id: item.id, status: item.status })));
      let current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).toArray()[0];
      if (!current) return;
      if (current.operation === 'delete') {
        const ids = new Set(listed.map(item => item.id));
        if (current.item_id) ids.add(current.item_id);
        for (const id of ids) { try { await items.delete(id); } catch (error) { if (!missing(error)) throw error; } }
        // Retain cleanup tombstones indefinitely with a bounded sweep interval.
        // This also catches an upload accepted remotely after a timeout/delete race.
        this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET item_id=NULL,retry_at=? WHERE chunk_id=?`,
          Date.now() + 300_000, row.chunk_id);
        return;
      }
      let item = listed[0];
      if (!item) {
        const source = this.storage.sql.exec<{ content: string }>(
          'SELECT content FROM markdown_memory_fts WHERE rowid=?', row.chunk_id).toArray()[0];
        if (!source) return;
        item = await disposeResult(items.upload(key, source.content, { metadata: {
          kind: KIND, organization_id: this.organizationId, owner: row.owner, path: row.path,
          revision: String(row.revision), markdown_chunk_id: String(row.chunk_id),
        } }), result => ({ id: result.id, status: result.status }));
      }
      // The document may have been deleted while upload was awaited. Record the
      // returned ID for cleanup, but never acknowledge a now-stale upload.
      this.storage.sql.exec('UPDATE markdown_memory_ai_items SET item_id=? WHERE chunk_id=?', item.id, row.chunk_id);
      current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).one();
      if (current.operation === 'upload' && item.status === 'completed') {
        this.storage.sql.exec('UPDATE markdown_memory_ai_items SET retry_at=NULL WHERE chunk_id=? AND operation=\'upload\'', row.chunk_id);
      } else if (current.operation === 'upload' && ['error', 'skipped', 'outdated'].includes(item.status)) {
        const handle = items.get(item.id);
        try { await disposeResult(handle.sync(), () => undefined); }
        finally { (handle as AiSearchItem & Partial<Disposable>)[Symbol.dispose]?.(); }
      }
    });
  }
  async search(owner: string, input: unknown) {
    // Canonical API validation precedes network access, including empty queries.
    this.store.search(owner, input);
    const { query, limit = 8 } = input as { query: string; limit?: number };
    let semantic: 'available' | 'unavailable' | 'error' = this.binding ? 'available' : 'unavailable';
    let ids: number[] = [];
    if (this.binding && query.trim()) {
      try {
        ids = await bounded(disposeResult(this.binding.search({ query, ai_search_options: {
          retrieval: { retrieval_type: 'vector', max_num_results: MAX_CANDIDATES, match_threshold: 0.3,
            filters: { kind: { $eq: KIND }, organization_id: { $eq: this.organizationId }, owner: { $eq: owner } },
            return_on_failure: false }, query_rewrite: { enabled: false }, cache: { enabled: false },
        } }), result => result.chunks.slice(0, MAX_CANDIDATES).flatMap(chunk => {
          const m = chunk.item.metadata;
          if (m?.kind !== KIND || m.organization_id !== this.organizationId || m.owner !== owner
            || typeof m.markdown_chunk_id !== 'string' || !/^\d+$/.test(m.markdown_chunk_id)) return [];
          const id = Number(m.markdown_chunk_id);
          // Bind the provider metadata to the immutable local revision as well.
          const row = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=? AND owner=?', id, owner).toArray()[0];
          return row && row.path === m.path && String(row.revision) === m.revision ? [id] : [];
        })));
      } catch { semantic = 'error'; }
    }
    // No awaits after this point: both candidate sets and returned text are read
    // from live canonical rows after network retrieval, excluding in-flight deletes.
    const lexical = this.store.search(owner, { query, limit: 20 }).results;
    const live = this.storage.sql.exec<Candidate>(`SELECT c.id AS chunk_id,c.path,CAST(f.revision AS INTEGER) AS revision,
      CAST(f.from_line AS INTEGER) AS from_line,CAST(f.to_line AS INTEGER) AS to_line,f.content AS snippet,0 AS score
      FROM markdown_memory_chunks c JOIN markdown_memory_fts f ON f.rowid=c.id
      JOIN markdown_memory_documents d ON d.owner=c.owner AND d.path=c.path AND d.revision=CAST(f.revision AS INTEGER)
      WHERE c.owner=? AND d.deleted=0 AND c.id IN (${ids.length ? ids.map(() => '?').join(',') : 'NULL'})`,
      owner, ...ids).toArray();
    const semanticById = new Map(live.map(hit => [hit.chunk_id, hit]));
    const candidates = new Map<string, Hit & { score: number }>();
    const identity = (hit: Hit) => JSON.stringify([hit.path, hit.revision, hit.from_line, hit.to_line]);
    const add = (hit: Hit, rank: number, weight: number) => {
      const key = identity(hit), previous = candidates.get(key);
      candidates.set(key, { ...hit, score: (previous?.score ?? 0) + weight / (60 + rank + 1) });
    };
    // Weighted reciprocal rank fusion (k=60): lexical .45, vector .55.
    // Daily notes have 30-day half life with a .35 floor; evergreen files never decay.
    lexical.forEach((hit, rank) => add(hit, rank, semantic === 'available' ? .45 : 1));
    [...new Set(ids)].forEach((id, rank) => { const hit = semanticById.get(id); if (hit) add(hit, rank, .55); });
    const now = Date.now();
    const ranked = [...candidates.values()].map(hit => {
      const date = /^memory\/(\d{4}-\d{2}-\d{2})/.exec(hit.path)?.[1];
      const days = date ? Math.max(0, (now - Date.parse(date + 'T00:00:00Z')) / 86_400_000) : 0;
      return { ...hit, score: hit.score * (date ? .35 + .65 * 2 ** (-days / 30) : 1) };
    });
    // MMR (.8 relevance, .2 token Jaccard redundancy), with an additional same
    // document penalty, prevents a long page from monopolizing bounded recall.
    const selected: typeof ranked = [];
    const tokens = (s: string) => new Set(s.toLowerCase().match(/[\p{L}\p{N}_]+/gu) ?? []);
    const overlap = (a: Hit, b: Hit) => {
      const x = tokens(a.snippet), y = tokens(b.snippet);
      const intersection = [...x].filter(token => y.has(token)).length;
      return Math.max(a.path === b.path ? .65 : 0, intersection / Math.max(1, x.size + y.size - intersection));
    };
    while (ranked.length && selected.length < limit) {
      ranked.sort((a, b) => {
        const score = (hit: typeof a) => .8 * hit.score * 61 - .2 * Math.max(0, ...selected.map(other => overlap(hit, other)));
        return score(b) - score(a) || a.path.localeCompare(b.path) || a.from_line - b.from_line;
      });
      selected.push(ranked.shift()!);
    }
    return { results: selected.map(({ score, ...hit }) => ({ path: hit.path, revision: hit.revision,
      from_line: hit.from_line, to_line: hit.to_line, snippet: hit.snippet.slice(0, 2048) })),
      retrieval: { mode: semantic === 'available' ? 'hybrid' as const : 'fts' as const, semantic } };
  }
}
