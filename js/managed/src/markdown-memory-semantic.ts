import { createHash } from 'node:crypto';
import { MARKDOWN_MEMORY_CLEANUP_HORIZON_MS, MarkdownMemoryStore, validateMarkdownMemoryOwner } from './markdown-memory';

type Item = { chunk_id: number; owner: string; path: string; revision: number; operation: string;
  item_id: string | null; attempts: number; retry_at: number | null;
  cleanup_until: number | null; cleanup_state: string | null; last_error: string | null;
  lease_until: number | null; lease_id: string | null };
type Hit = { path: string; revision: number; from_line: number; to_line: number; snippet: string };
type Candidate = Hit & { chunk_id: number; score: number };
const KIND = 'markdown_memory';
const MAX_CANDIDATES = 40;
const TIMEOUT_MS = 2500;
const LEASE_MS = 30_000;
const MAX_ATTEMPTS = 16;

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
  private readonly active = new Map<number, { id: string; until: number }>();
  constructor(private readonly storage: DurableObjectStorage, private readonly organizationId: string,
    private readonly binding?: AiSearchInstance) {
    this.store = new MarkdownMemoryStore(storage);
  }
  status(owner: string) {
    owner = validateMarkdownMemoryOwner(owner);
    const counts = this.storage.sql.exec<{ pending: number; failures: number; indexed: number; exhausted: number;
      cleanup_pending: number; cleanup_completed: number; cleanup_expired: number; next_retry_at: number | null }>(`
      SELECT COALESCE(SUM(operation='upload' AND retry_at IS NOT NULL),0) AS pending,
        COALESCE(SUM(last_error IS NOT NULL),0) AS failures,
        COALESCE(SUM(last_error='retry_exhausted'),0) AS exhausted,
        COALESCE(SUM(operation='upload' AND retry_at IS NULL AND last_error IS NULL),0) AS indexed,
        COALESCE(SUM(cleanup_state='pending'),0) AS cleanup_pending,
        COALESCE(SUM(cleanup_state='complete'),0) AS cleanup_completed,
        COALESCE(SUM(cleanup_state='expired'),0) AS cleanup_expired,
        MIN(CASE WHEN retry_at IS NOT NULL THEN MAX(retry_at,COALESCE(lease_until,0)) END) AS next_retry_at FROM markdown_memory_ai_items WHERE owner=?`, owner).one();
    return { enabled: Boolean(this.binding), pending: counts.pending, failures: counts.failures, indexed: counts.indexed, exhausted: counts.exhausted,
      next_retry_at: this.binding ? counts.next_retry_at : null, attempt_budget: MAX_ATTEMPTS, lease_ms: LEASE_MS,
      cleanup: { pending: counts.cleanup_pending, completed: counts.cleanup_completed,
        expired: counts.cleanup_expired, horizon_ms: MARKDOWN_MEMORY_CLEANUP_HORIZON_MS } };
  }
  nextRetryAt(): number | undefined {
    if (!this.binding) return undefined;
    const active = [...this.active.keys()];
    // A durable lease is also a watchdog. It survives eviction while an RPC is
    // outstanding, and every new lease consumes the persisted attempt budget.
    const rows = this.storage.sql.exec<{ chunk_id: number; retry_at: number }>(
      `SELECT chunk_id,MAX(retry_at,COALESCE(lease_until,0)) AS retry_at FROM markdown_memory_ai_items
        WHERE retry_at IS NOT NULL ${active.length >= 4 ? `AND chunk_id IN (${active.map(() => '?').join(',')})` : ''}
        ORDER BY retry_at LIMIT 5`, ...(active.length >= 4 ? active : []),
    ).toArray();
    return rows.length ? Math.min(...rows.map(row => Math.max(row.retry_at, this.active.get(row.chunk_id)?.until ?? 0))) : undefined;
  }
  drain(now = Date.now()): Promise<void> {
    if (!this.binding) return Promise.resolve();
    if (this.draining) return this.draining;
    const task = this.drainBatch(now).finally(() => { if (this.draining === task) this.draining = undefined; });
    this.draining = task;
    return task;
  }
  private async wake(): Promise<void> {
    const alarm = await this.storage.getAlarm();
    const next = this.nextRetryAt();
    if (next !== undefined && (alarm === null || next < alarm)) {
      await this.storage.setAlarm(Math.max(Date.now() + 1, next));
    }
  }
  private defer(row: Item, now: number, error: string | null): void {
    const current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).toArray()[0];
    if (!current || current.retry_at === null || current.lease_id !== row.lease_id) return;
    if (current.operation === 'delete' && now >= current.cleanup_until!) {
      // Keep an inspectable failed receipt, but stop scheduling failed cleanups.
      this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET retry_at=NULL,lease_until=NULL,lease_id=NULL,cleanup_state='expired',last_error=?
        WHERE chunk_id=?`, error ?? 'cleanup_expired', row.chunk_id);
      return;
    }
    const retry = now + Math.min(300_000, 1000 * 2 ** Math.min(current.attempts, 8));
    this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET retry_at=?,last_error=? WHERE chunk_id=?`,
      current.operation === 'delete' ? Math.min(retry, current.cleanup_until!) : retry, error, row.chunk_id);
  }
  private async drainBatch(now: number): Promise<void> {
    for (const [id, lease] of this.active) if (lease.until <= now) this.active.delete(id);
    // Retain failures for status and explicit revision changes, but never keep an
    // alarm alive forever for a provider that cannot settle or index an item.
    this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET retry_at=NULL,lease_until=NULL,lease_id=NULL,
      last_error='retry_exhausted',cleanup_state=CASE WHEN operation='delete' THEN 'expired' ELSE cleanup_state END
      WHERE attempts>=? AND retry_at IS NOT NULL AND COALESCE(lease_until,0)<=?`, MAX_ATTEMPTS, now);
    if (this.active.size >= 4) return;
    const active = [...this.active.keys()];
    const rows = this.storage.sql.exec<Item>(`SELECT * FROM markdown_memory_ai_items
      WHERE retry_at IS NOT NULL AND retry_at<=? AND COALESCE(lease_until,0)<=?
      AND chunk_id NOT IN (${active.length ? active.map(() => '?').join(',') : 'SELECT NULL WHERE 0'})
      ORDER BY retry_at,chunk_id LIMIT ?`, now, now, ...active, 4 - active.length).toArray();
    await Promise.all(rows.map(async row => {
      const lease = { id: crypto.randomUUID(), until: now + LEASE_MS };
      this.active.set(row.chunk_id, lease);
      row.lease_id = lease.id;
      this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET attempts=attempts+1,lease_id=?,lease_until=? WHERE chunk_id=?`,
        lease.id, lease.until, row.chunk_id);
      // Persist the watchdog before starting external work. A timeout or object
      // eviction cannot leave a durable pending row without a future wakeup.
      await this.wake();
      const project = this.project(row, now).catch(() => {
        // Never persist provider error strings: they can contain private content.
        this.defer(row, Math.max(now, Date.now()), 'provider_error');
      }).finally(async () => {
        if (this.active.get(row.chunk_id) === lease) this.active.delete(row.chunk_id);
        this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET lease_until=NULL,lease_id=NULL
          WHERE chunk_id=? AND lease_id=?`, row.chunk_id, lease.id);
        await this.wake();
      });
      try { await bounded(project, 5000); } catch {
        this.defer(row, Math.max(now, Date.now()), 'timeout');
      }
    }));
  }
  private live(row: Item): { content: string } | undefined {
    return this.storage.sql.exec<{ content: string }>(`SELECT f.content FROM markdown_memory_chunks c
      JOIN markdown_memory_fts f ON f.rowid=c.id AND f.owner=c.owner AND f.path=c.path
      JOIN markdown_memory_documents d ON d.owner=c.owner AND d.path=c.path AND d.revision=CAST(f.revision AS INTEGER)
      WHERE c.id=? AND c.owner=? AND c.path=? AND d.revision=? AND d.deleted=0 AND d.path<>'DREAMS.md'`,
      row.chunk_id, row.owner, row.path, row.revision).toArray()[0];
  }
  private queueDelete(row: Item, now: number, itemId = row.item_id): void {
    // Retain the receipt permanently (no prose). Even after a bounded cleanup has
    // completed, a late upload can reopen it with its returned remote identity.
    this.storage.sql.exec(`INSERT INTO markdown_memory_ai_items
      (chunk_id,owner,path,revision,operation,item_id,attempts,retry_at,cleanup_until,cleanup_state,last_error)
      VALUES(?,?,?,?,'delete',?,0,? ,?,'pending',NULL)
      ON CONFLICT(chunk_id) DO UPDATE SET operation='delete',item_id=COALESCE(excluded.item_id,item_id),
        attempts=0,retry_at=excluded.retry_at,cleanup_until=excluded.cleanup_until,cleanup_state='pending',last_error=NULL,lease_id=NULL,lease_until=NULL`,
      row.chunk_id, row.owner, row.path, row.revision, itemId, now, now + MARKDOWN_MEMORY_CLEANUP_HORIZON_MS);
  }
  private async project(row: Item, now: number): Promise<void> {
    const key = keyFor(this.organizationId, row);
    await itemsOperation(this.binding!, async items => {
      // Reconcile exact immutable keys before every upload, including ambiguous
      // timeouts. Returned snippets/metadata never become canonical memory text.
      // Exact key + source is supported by AiSearchListItemsParams and the Items
      // API: https://developers.cloudflare.com/ai-search/api/items/rest-api/
      const listed = await disposeResult(items.list({ key, source: 'builtin', per_page: 50 }), result => result.result.filter(item => item.key === key)
          .map(item => ({ id: item.id, status: item.status })));
      let current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).toArray()[0];
      if (!current || current.lease_id !== row.lease_id) return;
      const source = this.live(current);
      if (current.operation === 'upload' && !source) {
        this.queueDelete(current, Math.max(now, Date.now()));
        current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).one();
      }
      if (current.operation === 'delete') {
        const ids = new Set(listed.map(item => item.id));
        if (current.item_id) ids.add(current.item_id);
        for (const id of ids) { try { await items.delete(id); } catch (error) { if (!missing(error)) throw error; } }
        const completed = Math.max(now, Date.now()) >= current.cleanup_until!;
        this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET item_id=NULL,retry_at=?,cleanup_state=?,last_error=NULL
          WHERE chunk_id=? AND operation='delete' AND cleanup_until=? AND item_id IS ?`,
          completed ? null : Math.min(Math.max(now, Date.now()) + 300_000, current.cleanup_until!),
          completed ? 'complete' : 'pending', row.chunk_id, current.cleanup_until, current.item_id);
        return;
      }
      let item = listed[0];
      if (!item) {
        item = await disposeResult(items.upload(key, source!.content, { metadata: {
          kind: KIND, organization_id: this.organizationId, owner: row.owner, path: row.path,
          revision: String(row.revision), markdown_chunk_id: String(row.chunk_id),
        } }), result => ({ id: result.id, status: result.status }));
      }
      // Recheck after every awaited upload, even if a different instance finished
      // its cleanup horizon meanwhile. Late completion reopens durable deletion.
      current = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=?', row.chunk_id).toArray()[0];
      if (!current || current.operation === 'delete' || !this.live(row)) {
        this.queueDelete(row, Math.max(now, Date.now()), item.id);
        return;
      }
      if (current.lease_id !== row.lease_id) return;
      this.storage.sql.exec('UPDATE markdown_memory_ai_items SET item_id=? WHERE chunk_id=?', item.id, row.chunk_id);
      if (item.status === 'completed') {
        this.storage.sql.exec(`UPDATE markdown_memory_ai_items SET retry_at=NULL,last_error=NULL WHERE chunk_id=? AND operation='upload'`, row.chunk_id);
      } else {
        const failed = ['error', 'skipped', 'outdated'].includes(item.status);
        if (failed) {
          const handle = items.get(item.id);
          try { await disposeResult(handle.sync(), () => undefined); }
          finally { (handle as AiSearchItem & Partial<Disposable>)[Symbol.dispose]?.(); }
        }
        this.defer(row, Math.max(now, Date.now()), failed ? 'index_error' : null);
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
          if (!Number.isSafeInteger(id) || id <= 0) return [];
          // Bind the provider metadata to the immutable local revision as well.
          const row = this.storage.sql.exec<Item>('SELECT * FROM markdown_memory_ai_items WHERE chunk_id=? AND owner=?', id, owner).toArray()[0];
          return row && row.operation === 'upload' && row.path !== 'DREAMS.md'
            && row.path === m.path && String(row.revision) === m.revision ? [id] : [];
        })));
      } catch { semantic = 'error'; }
    }
    // No awaits after this point: both candidate sets and returned text are read
    // from live canonical rows after network retrieval, excluding in-flight deletes.
    const lexical = this.store.search(owner, { query, limit: 20 }).results;
    const live = this.storage.sql.exec<Candidate>(`SELECT c.id AS chunk_id,c.path,CAST(f.revision AS INTEGER) AS revision,
      CAST(f.from_line AS INTEGER) AS from_line,CAST(f.to_line AS INTEGER) AS to_line,f.content AS snippet,0 AS score
      FROM markdown_memory_chunks c JOIN markdown_memory_fts f ON f.rowid=c.id AND f.owner=c.owner AND f.path=c.path
      JOIN markdown_memory_documents d ON d.owner=c.owner AND d.path=c.path AND d.revision=CAST(f.revision AS INTEGER)
      WHERE c.owner=? AND d.deleted=0 AND c.path<>'DREAMS.md' AND c.id IN (${ids.length ? ids.map(() => '?').join(',') : 'NULL'})`,
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
    // Cover distinct documents before adding extra passages from a long page.
    // Within each pass, MMR (.8 relevance, .2 token Jaccard redundancy) penalizes
    // duplicated wording while preserving the fused relevance and decay order.
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
        const seen = (hit: Hit) => Number(selected.some(other => other.path === hit.path));
        return seen(a) - seen(b) || score(b) - score(a) || a.path.localeCompare(b.path) || a.from_line - b.from_line;
      });
      selected.push(ranked.shift()!);
    }
    return { results: selected.map(({ score, ...hit }) => ({ path: hit.path, revision: hit.revision,
      from_line: hit.from_line, to_line: hit.to_line, snippet: hit.snippet.slice(0, 2048) })),
      retrieval: { mode: semantic === 'available' ? 'hybrid' as const : 'fts' as const, semantic } };
  }
}
