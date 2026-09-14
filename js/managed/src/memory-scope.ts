import { initializeMemoryContent, memoryIdentityDigest, readMemoryContent, storeMemoryContent } from "./durable-memory-storage";
import { DurableObject } from "cloudflare:workers";
import {
  initializeHistoryStorage, storeHistorySegments, deleteHistorySegments, readHistoryText,
  type HistorySegment,
} from "./memory-history-storage";

import {
  DurableMemoryError,
  MEMORY_PROBATION_DURATION_MS,
  parseMemoryOperation,
  rankMemories,
  type MemoryDeleteResult,
  type MemoryKey,
  type MemoryOperation,
  type MemoryPutResult,
  type MemoryReadResult,
  type MemoryRecord,
  type MemoryScanResult,
} from "./durable-memory";

import {
  HistorySearchError,
  MAX_HISTORY_SEARCH_LIMIT,
  groupHistoryCitations,
  historyVectorRetrieval,
  historyFtsQuery,
  historySearchTerms,
  isAcceptedHistoryLexicalMatch,
  isExactHistoryIdentifierQuery,
  isRecord,
  parseHistoryFindSessionsInput,
  parseHistoryReadSessionInput,
  promptInputText,
  type HistoryFindSessionsInput,
  type HistoryFindSessionsResponse,
  type HistoryProjection,
  type HistoryReadSessionResponse,
  type HistorySearchHit,
} from "./history-search";

const ORGANIZATION_ASSERTION = "x-nanocodex-organization-id";
const TEAM_ASSERTION = "x-nanocodex-team-id";
const SUBJECT_ASSERTION = "x-nanocodex-subject-id";
const MEMORY_MUTATION_ASSERTION = "x-nanocodex-memory-mutation";
const MEMORY_SCAN_RECEIPT_MS = 30 * 60 * 1_000;
const SESSION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[78][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TURN_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const MAX_AI_RETRY_DELAY_MS = 60_000;
const VECTOR_SEARCH_CACHE_MS = 30_000;
const EMPTY_VECTOR_SEARCH_CACHE_MS = 1_000;

export interface MemoryScopeEnv {
  HISTORY_AI_SEARCH?: AiSearchInstance;
}

type MemoryTurnRow = {
  segment_id: string;
  thread_id: string;
  title: string;
  turn_id: string;
  source_cursor: string;
  created_at: number;
};

type DurableMemoryRow = {
  id: number;
  version: number;
  owner_team_id: string;
  content_json: string;
  identity_digest: string;
  created_at_ms: number;
  updated_at_ms: number;
  last_scanned_at_ms: number | null;
  scan_count: number;
  last_used_at_ms: number | null;
  use_count: number;
  probation_until_ms: number | null;
};

type RankedMemoryTurnRow = MemoryTurnRow & { content: string; rank: number; semantic_score?: number };

type AiOutboxRow = {
  operation_id: string;
  operation: "upsert" | "delete";
  segment_id: string;
  ai_item_id: string | null;
  attempt_count: number;
  retry_at: number;
};

type DisposableAiSearchObject<T extends object> = T & Disposable;

type AiSearchItemState = Pick<AiSearchItemInfo, "id" | "status">;

const copyAiSearchItemState = (item: AiSearchItemInfo): AiSearchItemState => ({
  id: item.id,
  status: item.status,
});

export async function withAiSearchItems<T>(
  instance: Pick<AiSearchInstance, "items">,
  operation: (items: AiSearchItems) => Promise<T>,
): Promise<T> {
  const items = instance.items as AiSearchItems & Partial<Disposable>;
  try {
    return await operation(items);
  } finally {
    const dispose = items[Symbol.dispose];
    if (typeof dispose === "function") dispose.call(items);
  }
}

export async function withAiSearchResult<T extends object, R>(
  result: Promise<T>,
  operation: (value: T) => R | Promise<R>,
): Promise<R> {
  const value = await result as DisposableAiSearchObject<T>;
  try {
    return await operation(value);
  } finally {
    value[Symbol.dispose]();
  }
}

export async function withAiSearchItem<T>(
  items: Pick<AiSearchItems, "get">,
  itemId: string,
  operation: (item: AiSearchItem) => Promise<T>,
): Promise<T> {
  const item = items.get(itemId) as DisposableAiSearchObject<AiSearchItem>;
  try {
    return await operation(item);
  } finally {
    item[Symbol.dispose]();
  }
}

const json = (body: unknown, init: ResponseInit = {}) => Response.json(body, {
  ...init,
  headers: { "cache-control": "no-store", ...init.headers },
});

export class MemoryScope extends DurableObject<MemoryScopeEnv> {
  #aiTask?: Promise<void>;
  #vectorSearches = new Map<string, Promise<RankedMemoryTurnRow[]>>();
  #vectorCache = new Map<string, { expiresAt: number; rows: RankedMemoryTurnRow[] }>();

  constructor(ctx: DurableObjectState, env: MemoryScopeEnv) {
    super(ctx, env);
    this.ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS memory_scope_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        organization_id TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS memory_threads (
        thread_id TEXT PRIMARY KEY,
        team_id TEXT NOT NULL,
        title TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS memory_tombstones (
        thread_id TEXT PRIMARY KEY,
        deleted_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS memory_scan_receipts (
        subject_id TEXT PRIMARY KEY,
        expires_at_ms INTEGER NOT NULL
      );
    `);
    initializeHistoryStorage(this.ctx.storage, this.env.HISTORY_AI_SEARCH !== undefined);
    initializeMemoryContent(this.ctx.storage);
    this.ctx.blockConcurrencyWhile(async () => {
      this.#scheduleAiOutbox();
    });
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const assertedOrganization = request.headers.get(ORGANIZATION_ASSERTION);
    if (request.method === "PUT" && url.pathname === "/initialize") {
      if (assertedOrganization === null) return json({ error: "not_found" }, { status: 404 });
      return this.#initialize(assertedOrganization);
    }
    if (!this.#authorized(assertedOrganization)) return json({ error: "not_found" }, { status: 404 });
    const assertedTeam = request.headers.get(TEAM_ASSERTION);
    if (assertedTeam === null) return json({ error: "not_found" }, { status: 404 });
    try {
      if (request.method === "POST" && url.pathname === "/project") {
        const projection = await parseJsonBody<HistoryProjection>(request);
        this.#project(projection, assertedTeam);
        this.#scheduleAiOutbox();
        return new Response(null, { status: 204 });
      }
      const threadMatch = url.pathname.match(/^\/threads\/([0-9a-f-]+)$/);
      if (request.method === "DELETE" && threadMatch) {
        if (!SESSION_ID.test(threadMatch[1]!)) {
          throw new HistorySearchError(400, "invalid_thread_id", "invalid thread id");
        }
        this.#deleteThread(threadMatch[1]!, assertedTeam);
        this.#scheduleAiOutbox();
        return new Response(null, { status: 204 });
      }
      if (request.method === "POST" && url.pathname === "/search") {
        const input = parseHistoryFindSessionsInput(await parseJsonBody<unknown>(request));
        return json(await this.#findSessions(input, assertedTeam));
      }
      if (request.method === "POST" && url.pathname === "/read") {
        const input = parseHistoryReadSessionInput(await parseJsonBody<unknown>(request));
        const rows = this.#readThread(
          input.session_id,
          input.turn_ids,
          MAX_HISTORY_SEARCH_LIMIT,
          assertedTeam,
        );
        const turns = rows.map((row) => ({
          thread_id: row.thread_id,
          title: row.title,
          turn_id: row.turn_id,
          cursor: row.source_cursor,
          user: readHistoryText(this.ctx.storage, row.segment_id, "user"),
          assistant: readHistoryText(this.ctx.storage, row.segment_id, "assistant"),
        }));
        const citations = groupHistoryCitations(rows.map((row) => ({
          thread_id: row.thread_id,
          title: row.title,
          turn_id: row.turn_id,
          cursor: row.source_cursor,
        })));
        return json({ turns, citations } satisfies HistoryReadSessionResponse);
      }
      if (request.method === "GET" && url.pathname === "/memories") {
        return this.#listMemories(assertedTeam);
      }
      if (request.method === "POST" && url.pathname === "/memory") {
        const operation = parseMemoryOperation(await parseJsonBody<unknown>(request));
        const mutating = operation.operation === "put" || operation.operation === "delete";
        if (mutating && request.headers.get(MEMORY_MUTATION_ASSERTION) !== "1") {
          return json({ error: "memory_read_only", message: "memory mutation is not authorized" }, { status: 403 });
        }
        const subjectId = request.headers.get(SUBJECT_ASSERTION);
        if (subjectId === null) return json({ error: "not_found" }, { status: 404 });
        return json(this.#memory(operation, assertedTeam, subjectId));
      }
      return json({ error: "not_found" }, { status: 404 });
    } catch (error) {
      if (error instanceof HistorySearchError) {
        return json({ error: error.code, message: error.message }, { status: error.status });
      }
      if (error instanceof DurableMemoryError) {
        return json({ error: error.code, message: error.message }, {
          status: error.code === "memory_conflict" || error.code === "memory_duplicate" ? 409
            : error.code === "memory_not_found" ? 404
              : error.code === "memory_secret_rejected" ? 422
                : 400,
        });
      }
      console.error({ type: "memory_scope.request_failed", error_kind: errorKind(error) });
      return json({ error: "memory_scope_failed", message: errorMessage(error) }, { status: 500 });
    }
  }

  async alarm(): Promise<void> {
    if (this.#aiTask) await this.#aiTask.catch(() => {});
    else await this.#drainAiOutbox();
    await this.#scheduleNextAlarm();
  }

  #initialize(organizationId: string): Response {
    const current = this.#organizationId();
    if (current !== undefined && current !== organizationId) return json({ error: "not_found" }, { status: 404 });
    if (current === undefined) {
      this.ctx.storage.sql.exec(
        "INSERT INTO memory_scope_state (singleton, organization_id, created_at) VALUES (1, ?, ?)",
        organizationId,
        Date.now(),
      );
    }
    return new Response(null, { status: 204 });
  }

  #authorized(assertedOrganization: string | null): boolean {
    return assertedOrganization !== null && assertedOrganization === this.#organizationId();
  }

  #organizationId(): string | undefined {
    return this.ctx.storage.sql.exec<{ organization_id: string }>(
      "SELECT organization_id FROM memory_scope_state WHERE singleton = 1",
    ).toArray()[0]?.organization_id;
  }

  #project(projection: HistoryProjection, teamId: string): void {
    if (!isRecord(projection)
      || typeof projection.thread_id !== "string" || !SESSION_ID.test(projection.thread_id)
      || typeof projection.turn_id !== "string" || !TURN_ID.test(projection.turn_id)
      || typeof projection.cursor !== "string" || !/^\d+$/.test(projection.cursor)
      || typeof projection.title !== "string"
      || typeof projection.final_message !== "string"
      || !Number.isSafeInteger(projection.created_at)) {
      throw new HistorySearchError(400, "invalid_projection", "invalid history projection");
    }
    const userText = promptInputText(projection.input);
    const segmentId = `${projection.thread_id}:${projection.turn_id}`;
    this.ctx.storage.transactionSync(() => {
      if (this.ctx.storage.sql.exec(
        "SELECT 1 AS present FROM memory_tombstones WHERE thread_id = ?",
        projection.thread_id,
      ).toArray().length > 0) return;
      const retained = this.ctx.storage.sql.exec<{ source_cursor: string }>(
        "SELECT CAST(source_cursor AS TEXT) AS source_cursor FROM memory_turns WHERE segment_id = ?", segmentId,
      ).toArray()[0];
      // Replayed/out-of-order projections must not replace content or pending AI
      // work. Every accepted generation gets immutable segment/upload identities.
      if (retained && BigInt(retained.source_cursor) >= BigInt(projection.cursor)) return;
      const owner = this.ctx.storage.sql.exec<{ team_id: string }>(
        "SELECT team_id FROM memory_threads WHERE thread_id = ?", projection.thread_id,
      ).toArray()[0];
      if (owner && owner.team_id !== teamId) throw new HistorySearchError(404, "not_found", "session was not found");
      this.ctx.storage.sql.exec(
        `INSERT INTO memory_threads (thread_id, team_id, title, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(thread_id) DO UPDATE SET title = excluded.title,
           updated_at = MAX(memory_threads.updated_at, excluded.updated_at)`,
        projection.thread_id, teamId, projection.title, projection.created_at, projection.created_at,
      );
      deleteHistorySegments(this.ctx.storage, segmentId, this.env.HISTORY_AI_SEARCH !== undefined);
      this.ctx.storage.sql.exec(
        `INSERT INTO memory_turns (segment_id, thread_id, turn_id, source_cursor, created_at)
         VALUES (?, ?, ?, CAST(? AS INTEGER), ?)
         ON CONFLICT(segment_id) DO UPDATE SET source_cursor = excluded.source_cursor,
           created_at = excluded.created_at`,
        segmentId, projection.thread_id, projection.turn_id, projection.cursor, projection.created_at,
      );
      storeHistorySegments(this.ctx.storage, segmentId, userText, projection.final_message,
        this.env.HISTORY_AI_SEARCH !== undefined);
    });
    this.#vectorCache.clear();
  }

  #deleteThread(threadId: string, teamId: string): void {
    this.ctx.storage.transactionSync(() => {
      const owner = this.ctx.storage.sql.exec<{ team_id: string }>(
        "SELECT team_id FROM memory_threads WHERE thread_id = ?",
        threadId,
      ).toArray()[0];
      if (owner !== undefined && owner.team_id !== teamId) {
        throw new HistorySearchError(404, "not_found", "session was not found");
      }
      this.ctx.storage.sql.exec(
        "INSERT OR REPLACE INTO memory_tombstones (thread_id, deleted_at) VALUES (?, ?)", threadId, Date.now(),
      );
      for (const row of this.ctx.storage.sql.exec<{ segment_id: string }>(
        "SELECT segment_id FROM memory_turns WHERE thread_id = ?", threadId,
      )) deleteHistorySegments(this.ctx.storage, row.segment_id, this.env.HISTORY_AI_SEARCH !== undefined);
      this.ctx.storage.sql.exec("DELETE FROM memory_threads WHERE thread_id = ?", threadId);
    });
    this.#vectorCache.clear();
  }

  async #findSessions(
    input: HistoryFindSessionsInput,
    teamId: string,
  ): Promise<HistoryFindSessionsResponse> {
    const local = this.#localSearch(input.query, input.limit, teamId);
    let rows = local;
    if (this.env.HISTORY_AI_SEARCH !== undefined && !isExactHistoryIdentifierQuery(input.query)) {
      try {
        const vector = await this.#sharedVectorSearch(input.query, input.limit, teamId);
        // Prose queries are routed here for semantic retrieval; exact
        // identifiers already take the FTS-only path above. Keep the semantic
        // winner first and use lexical rows to broaden the remainder.
        rows = interleaveRankedRows(vector, local, input.limit);
      } catch (error) {
        // SQLite remains authoritative while uploads are pending or the
        // external index is unavailable.
        console.warn({
          type: "memory_scope.ai_search_query_failed",
          error_kind: errorKind(error),
          fallback: "local_fts",
        });
      }
    }
    const results = rows.map((row) => memoryHit(row, input.query));
    return {
      query: input.query,
      results,
      citations: groupHistoryCitations(results),
    };
  }

  #localSearch(query: string, limit: number, teamId: string): RankedMemoryTurnRow[] {
    const match = historyFtsQuery(query);
    if (!match) return [];
    const candidateLimit = Math.min(50, Math.max(limit, limit * 3));
    const candidates = this.ctx.storage.sql.exec<RankedMemoryTurnRow>(
      `WITH hits AS MATERIALIZED (
         SELECT s.turn_segment_id, bm25(memory_segments_fts) AS rank
         FROM memory_segments_fts JOIN memory_segments s ON s.rowid = memory_segments_fts.rowid
         JOIN memory_turns m ON m.segment_id = s.turn_segment_id
         JOIN memory_threads t ON t.thread_id = m.thread_id
         WHERE memory_segments_fts MATCH ? AND t.team_id = ?
       )
       SELECT m.segment_id, m.thread_id, t.title, m.turn_id,
              CAST(m.source_cursor AS TEXT) AS source_cursor, m.created_at, MIN(h.rank) AS rank
       FROM hits h JOIN memory_turns m ON m.segment_id = h.turn_segment_id
       JOIN memory_threads t ON t.thread_id = m.thread_id
       GROUP BY m.segment_id ORDER BY rank, m.created_at DESC LIMIT ?`,
      match, teamId, candidateLimit,
    ).toArray();
    const accepted: RankedMemoryTurnRow[] = [];
    const terms = historySearchTerms(query);
    for (const candidate of candidates) {
      const matched = new Set<string>();
      let phraseAccepted = false;
      let preview: string | undefined;
      let adjacentTail = "";
      let previousField = "";
      let previousWhitespace = false;
      for (const segment of this.ctx.storage.sql.exec<Pick<HistorySegment, "content" | "field" | "overlap">>(
        "SELECT content, field, overlap FROM memory_segments WHERE turn_segment_id = ? ORDER BY field, chunk_index",
        candidate.segment_id,
      )) {
        const content = segment.content;
        if (preview === undefined && terms.some((term) => content.toLocaleLowerCase().includes(term))) {
          preview = snippet(content, query);
        }
        if (terms.length <= 2 || isExactHistoryIdentifierQuery(query)) {
          // Preserve phrase matching even when arbitrarily much whitespace spans
          // storage chunks. Exact identifiers use the overlapping raw content.
          const original = content.slice(segment.overlap);
          const normalized = original.normalize("NFKC").replace(/[_-]+/gu, " ").match(/[\p{L}\p{N}]+/gu) ?? [];
          const boundary = previousField === segment.field && previousWhitespace && /^\s/u.test(original)
            && !isExactHistoryIdentifierQuery(query)
            ? `${adjacentTail} ${normalized[0] ?? ""}` : "";
          if (isAcceptedHistoryLexicalMatch(query, content) || (boundary && isAcceptedHistoryLexicalMatch(query, boundary))) {
            preview = snippet(content, query);
            phraseAccepted = true;
            break;
          }
          if (normalized.length) adjacentTail = normalized.at(-1)!;
          previousField = segment.field;
          previousWhitespace = /\s$/u.test(original);
        } else {
          for (const term of terms) if (!matched.has(term) && isAcceptedHistoryLexicalMatch(term, content)) matched.add(term);
          if (matched.size >= Math.max(2, Math.ceil(terms.length * 0.6))) break;
        }
      }
      if (phraseAccepted || (terms.length > 2 && !isExactHistoryIdentifierQuery(query)
        && matched.size >= Math.max(2, Math.ceil(terms.length * 0.6)))) {
        accepted.push({ ...candidate, content: preview ?? "" });
        if (accepted.length === limit) break;
      }
    }
    return accepted;
  }

  async #sharedVectorSearch(
    query: string,
    limit: number,
    teamId: string,
  ): Promise<RankedMemoryTurnRow[]> {
    const key = JSON.stringify([teamId, query, limit]);
    const cached = this.#vectorCache.get(key);
    if (cached !== undefined) {
      if (cached.expiresAt > Date.now()) return cached.rows;
      this.#vectorCache.delete(key);
    }
    let task = this.#vectorSearches.get(key);
    if (task === undefined) {
      task = this.#autoVectorSearch(query, limit, teamId);
      this.#vectorSearches.set(key, task);
    }
    try {
      const rows = await task;
      this.#vectorCache.set(key, {
        expiresAt: Date.now() + (rows.length === 0
          ? EMPTY_VECTOR_SEARCH_CACHE_MS
          : VECTOR_SEARCH_CACHE_MS),
        rows,
      });
      return rows;
    } finally {
      if (this.#vectorSearches.get(key) === task) this.#vectorSearches.delete(key);
    }
  }

  async #autoVectorSearch(
    query: string,
    limit: number,
    teamId: string,
  ): Promise<RankedMemoryTurnRow[]> {
    const organizationId = this.#organizationId();
    if (organizationId === undefined) return [];
    const candidates = await withAiSearchResult(
      this.env.HISTORY_AI_SEARCH!.search({
        query,
        ai_search_options: {
          retrieval: historyVectorRetrieval(organizationId, teamId, limit),
          query_rewrite: { enabled: false },
          // Vector similarity is the broad candidate generator. The reranker is
          // necessary for near-neighbor histories that share most nouns but
          // differ on one decisive actor, location, or value.
          reranking: {
            enabled: true,
            model: "@cf/baai/bge-reranker-base",
            // Retrieval already rejects weak vector matches. Reranker scores are
            // model-specific ordering signals, so its default 0.4 cutoff can
            // incorrectly discard every otherwise valid candidate.
            match_threshold: 0,
          },
          // Memory is mutable and AI Search may accept an item before its
          // filtered vector view is complete. Caching that early result makes a
          // newly projected turn invisible for subsequent identical queries.
          cache: { enabled: false },
        },
      }),
      (searched) => searched.chunks.flatMap((chunk) => {
        const segmentId = chunk.item.metadata?.segment_id;
        const score = chunk.scoring_details?.reranking_score
          ?? chunk.scoring_details?.vector_score
          ?? chunk.score;
        return typeof segmentId === "string"
          && Number.isFinite(score)
          ? [{ segmentId, score }]
          : [];
      }),
    );
    const bySegment = new Map<string, { segmentId: string; score: number }>();
    for (const candidate of candidates) {
      const current = bySegment.get(candidate.segmentId);
      if (current === undefined || candidate.score > current.score) {
        bySegment.set(candidate.segmentId, candidate);
      }
    }
    const unique = [...bySegment.values()];
    if (unique.length === 0) return [];
    const placeholders = unique.map(() => "?").join(", ");
    const rows = this.ctx.storage.sql.exec<MemoryTurnRow & { content: string; matched_segment_id: string }>(
      `SELECT m.segment_id, m.thread_id, t.title, m.turn_id,
              CAST(m.source_cursor AS TEXT) AS source_cursor,
              s.content, m.created_at, s.segment_id AS matched_segment_id
       FROM memory_segments s JOIN memory_turns m ON m.segment_id = s.turn_segment_id
       JOIN memory_threads t ON t.thread_id = m.thread_id
       WHERE s.segment_id IN (${placeholders}) AND t.team_id = ?`,
      ...unique.map(({ segmentId }) => segmentId),
      teamId,
    );
    // Retain only display snippets, not up to `limit` full segment bodies.
    const byId = new Map<string, MemoryTurnRow & { content: string }>();
    for (const row of rows) byId.set(row.matched_segment_id, { ...row, content: snippet(row.content, query) });
    const seen = new Set<string>();
    return unique.flatMap(({ segmentId, score }) => {
      const row = byId.get(segmentId);
      if (row === undefined || seen.has(row.segment_id)) return [];
      seen.add(row.segment_id);
      return [{ ...row, content: snippet(row.content, query), rank: -score, semantic_score: score }];
    }).slice(0, limit);
  }

  #readThread(
    threadId: string,
    turnIds: readonly string[] | undefined,
    limit: number,
    teamId: string,
  ): MemoryTurnRow[] {
    if (turnIds !== undefined && turnIds.length > 0) {
      const selected = [...new Set(turnIds)].slice(0, MAX_HISTORY_SEARCH_LIMIT);
      const placeholders = selected.map(() => "?").join(", ");
      return this.ctx.storage.sql.exec<MemoryTurnRow>(
        `SELECT m.segment_id, m.thread_id, t.title, m.turn_id,
                CAST(m.source_cursor AS TEXT) AS source_cursor,
                m.created_at
         FROM memory_turns m
         JOIN memory_threads t ON t.thread_id = m.thread_id
         WHERE m.thread_id = ? AND t.team_id = ? AND m.turn_id IN (${placeholders})
         ORDER BY m.created_at, m.rowid
         LIMIT ?`,
        threadId,
        teamId,
        ...selected,
        limit,
      ).toArray();
    }
    return this.ctx.storage.sql.exec<MemoryTurnRow>(
      `SELECT * FROM (
         SELECT m.segment_id, m.thread_id, t.title, m.turn_id,
                CAST(m.source_cursor AS TEXT) AS source_cursor,
                m.created_at
         FROM memory_turns m
         JOIN memory_threads t ON t.thread_id = m.thread_id
         WHERE m.thread_id = ? AND t.team_id = ?
         ORDER BY m.created_at DESC, m.rowid DESC
         LIMIT ?
       ) ORDER BY created_at`,
      threadId,
      teamId,
      limit,
    ).toArray();
  }

  #memory(operation: MemoryOperation, teamId: string, subjectId: string) {
    switch (operation.operation) {
      case "scan": {
        const result = this.#scanMemories(operation.query, operation.limit, teamId);
        this.ctx.storage.sql.exec(
          `INSERT INTO memory_scan_receipts (subject_id, expires_at_ms) VALUES (?, ?)
           ON CONFLICT(subject_id) DO UPDATE SET expires_at_ms = excluded.expires_at_ms`,
          subjectId,
          Date.now() + MEMORY_SCAN_RECEIPT_MS,
        );
        return result;
      }
      case "read": return this.#readMemories(operation.keys, teamId);
      case "put": {
        const receipt = this.ctx.storage.sql.exec<{ expires_at_ms: number }>(
          "SELECT expires_at_ms FROM memory_scan_receipts WHERE subject_id = ?",
          subjectId,
        ).toArray()[0];
        this.ctx.storage.sql.exec("DELETE FROM memory_scan_receipts WHERE subject_id = ?", subjectId);
        if (!receipt || receipt.expires_at_ms < Date.now()) {
          throw new DurableMemoryError("memory_scan_required", "scan memory before storing a conclusion");
        }
        return this.#putMemory(operation.content, operation.replace, teamId);
      }
      case "delete": return this.#deleteMemory(operation.key, teamId);
    }
  }

  #scanMemories(query: string, limit: number, teamId: string): MemoryScanResult {
    const now = Date.now();
    return this.ctx.storage.transactionSync(() => {
      this.#pruneMemories(now);
      const storage = this.ctx.storage;
      const scan = rankMemories(query, function* () {
        for (const row of storage.sql.exec<DurableMemoryRow>(
          "SELECT * FROM durable_memories WHERE owner_team_id = ? ORDER BY id", teamId,
        )) yield memoryRecord(row, storage);
      }, limit);
      for (const candidate of scan.candidates) {
        this.ctx.storage.sql.exec(
          `UPDATE durable_memories
           SET last_scanned_at_ms = ?,
               scan_count = CASE WHEN scan_count < 9223372036854775807 THEN scan_count + 1 ELSE scan_count END
           WHERE id = ? AND version = ? AND owner_team_id = ?`,
          now,
          candidate.key.id,
          candidate.key.version,
          teamId,
        );
      }
      return { operation: "scan", ...scan };
    });
  }

  #readMemories(keys: readonly MemoryKey[], teamId: string): MemoryReadResult {
    const now = Date.now();
    return this.ctx.storage.transactionSync(() => {
      this.#pruneMemories(now);
      const memories: MemoryRecord[] = [];
      for (const key of keys) {
        const row = this.ctx.storage.sql.exec<DurableMemoryRow>(
          `SELECT * FROM durable_memories
           WHERE id = ? AND version = ? AND owner_team_id = ?`,
          key.id,
          key.version,
          teamId,
        ).toArray()[0];
        if (!row) continue;
        this.ctx.storage.sql.exec(
          `UPDATE durable_memories
           SET last_used_at_ms = ?, probation_until_ms = NULL,
               use_count = CASE WHEN use_count < 9223372036854775807 THEN use_count + 1 ELSE use_count END
           WHERE id = ? AND version = ? AND owner_team_id = ?`,
          now,
          key.id,
          key.version,
          teamId,
        );
        memories.push(memoryRecord({
          ...row,
          last_used_at_ms: now,
          use_count: row.use_count + 1,
          probation_until_ms: null,
        }, this.ctx.storage));
      }
      return { operation: "read", memories };
    });
  }

  #listMemories(teamId: string): Response {
    this.#pruneMemories(Date.now());
    const storage = this.ctx.storage;
    const maxId = storage.sql.exec<{ id: number }>(
      "SELECT COALESCE(MAX(id), 0) AS id FROM durable_memories WHERE owner_team_id = ?", teamId,
    ).one().id;
    const chunks = (function* () {
      yield '{"memories":[';
      let created = -1;
      let id = 0;
      let first = true;
      while (true) {
        const rows = storage.sql.exec<DurableMemoryRow>(
          `SELECT * FROM durable_memories WHERE owner_team_id = ? AND id <= ?
           AND (created_at_ms, id) > (?, ?) ORDER BY created_at_ms, id LIMIT 64`,
          teamId, maxId, created, id,
        ).toArray();
        if (rows.length === 0) break;
        for (const row of rows) {
          yield `${first ? "" : ","}${JSON.stringify(memoryRecord(row, storage))}`;
          first = false;
        }
        created = rows.at(-1)!.created_at_ms;
        id = rows.at(-1)!.id;
      }
      yield "]}";
    })();
    const encoder = new TextEncoder();
    return new Response(new ReadableStream<Uint8Array>({
      pull(controller) {
        const next = chunks.next();
        if (next.done) controller.close();
        else controller.enqueue(encoder.encode(next.value));
      },
      cancel() { chunks.return(); },
    }), { headers: { "content-type": "application/json", "cache-control": "no-store" } });
  }

  #putMemory(content: string, replace: MemoryKey | undefined, teamId: string): MemoryPutResult {
    if (containsLikelySecret(content)) {
      throw new DurableMemoryError("memory_secret_rejected", "memory content was rejected as a likely secret");
    }
    const identity = memoryIdentityDigest(content);
    const now = Date.now();
    const probationUntil = now + MEMORY_PROBATION_DURATION_MS;
    return this.ctx.storage.transactionSync(() => {
      this.#pruneMemories(now);
      const duplicate = this.ctx.storage.sql.exec<{ id: number }>(
        "SELECT id FROM durable_memories WHERE identity_digest = ? AND (? IS NULL OR id != ?)",
        identity,
        replace?.id ?? null,
        replace?.id ?? null,
      ).toArray()[0];
      if (duplicate) {
        throw new DurableMemoryError("memory_duplicate", "an equivalent memory already exists");
      }
      if (replace) {
        const current = this.ctx.storage.sql.exec<DurableMemoryRow>(
          "SELECT * FROM durable_memories WHERE id = ? AND owner_team_id = ?",
          replace.id,
          teamId,
        ).toArray()[0];
        if (!current) throw new DurableMemoryError("memory_not_found", "memory was not found");
        if (current.version !== replace.version) {
          throw new DurableMemoryError("memory_conflict", "memory changed since it was read");
        }
        this.ctx.storage.sql.exec(
          `UPDATE durable_memories
           SET version = version + 1, identity_digest = ?, updated_at_ms = ?,
               last_scanned_at_ms = NULL, scan_count = 0,
               last_used_at_ms = NULL, use_count = 0, probation_until_ms = ?
           WHERE id = ? AND version = ? AND owner_team_id = ?`,
          identity,
          now,
          probationUntil,
          replace.id,
          replace.version,
          teamId,
        );
        storeMemoryContent(this.ctx.storage, replace.id, content);
        const updated = this.ctx.storage.sql.exec<DurableMemoryRow>(
          "SELECT * FROM durable_memories WHERE id = ?",
          replace.id,
        ).toArray()[0]!;
        return { operation: "put", memory: memoryRecord(updated, this.ctx.storage, content), replaced: true };
      }

      this.ctx.storage.sql.exec(
        `INSERT INTO durable_memories (
           version, owner_team_id, content_json, identity_digest,
           created_at_ms, updated_at_ms, probation_until_ms
         ) VALUES (1, ?, ?, ?, ?, ?, ?)`,
        teamId,
        "",
        identity,
        now,
        now,
        probationUntil,
      );
      const inserted = this.ctx.storage.sql.exec<DurableMemoryRow>(
        "SELECT * FROM durable_memories WHERE identity_digest = ?",
        identity,
      ).toArray()[0]!;
      storeMemoryContent(this.ctx.storage, inserted.id, content);
      return { operation: "put", memory: memoryRecord(inserted, this.ctx.storage, content), replaced: false };
    });
  }

  #deleteMemory(key: MemoryKey, teamId: string): MemoryDeleteResult {
    return this.ctx.storage.transactionSync(() => {
      const current = this.ctx.storage.sql.exec<Pick<DurableMemoryRow, "version">>(
        "SELECT version FROM durable_memories WHERE id = ? AND owner_team_id = ?",
        key.id,
        teamId,
      ).toArray()[0];
      if (!current) throw new DurableMemoryError("memory_not_found", "memory was not found");
      if (current.version !== key.version) {
        throw new DurableMemoryError("memory_conflict", "memory changed since it was read");
      }
      this.ctx.storage.sql.exec(
        "DELETE FROM durable_memories WHERE id = ? AND version = ? AND owner_team_id = ?",
        key.id,
        key.version,
        teamId,
      );
      return { operation: "delete", key };
    });
  }

  #pruneMemories(now: number): void {
    this.ctx.storage.sql.exec(
      "DELETE FROM durable_memories WHERE probation_until_ms <= ? AND use_count = 0",
      now,
    );
  }

  #scheduleAiOutbox(): void {
    if (this.env.HISTORY_AI_SEARCH === undefined) return;
    this.ctx.waitUntil(this.ctx.storage.setAlarm(Date.now() + 1).catch((error) => {
      console.warn({ type: "memory_scope.ai_outbox_schedule_failed", error_kind: errorKind(error) });
    }));
    if (this.#aiTask) return;
    const task = this.#drainAiOutbox();
    this.#aiTask = task;
    void task.finally(() => {
      if (this.#aiTask === task) this.#aiTask = undefined;
    }).catch(() => {});
    this.ctx.waitUntil(task.catch(async (error) => {
      console.warn({ type: "memory_scope.ai_projection_failed", error_kind: errorKind(error) });
      await this.#scheduleNextAlarm();
    }));
  }

  async #drainAiOutbox(): Promise<void> {
    if (this.env.HISTORY_AI_SEARCH === undefined) return;
    while (true) {
      const rows = this.ctx.storage.sql.exec<AiOutboxRow>(
        `SELECT operation_id, operation, segment_id, ai_item_id,
                attempt_count, retry_at
         FROM memory_ai_outbox
         WHERE retry_at <= ?
         ORDER BY rowid
         LIMIT 16`,
        Date.now(),
      ).toArray();
      if (rows.length === 0) break;
      for (const row of rows) {
        try {
          if (row.operation === "delete") {
            const deleted = await this.#deleteAiItem(row);
            if (!deleted) {
              this.#deferAiOperation(row);
              continue;
            }
          } else {
            const current = this.ctx.storage.sql.exec<HistorySegment & { team_id: string }>(
              `SELECT s.*, t.team_id FROM memory_segments s
               JOIN memory_turns m ON m.segment_id = s.turn_segment_id
               JOIN memory_threads t ON t.thread_id = m.thread_id WHERE s.segment_id = ?`, row.segment_id,
            ).toArray()[0];
            const name = `${row.segment_id}.md`;
            if (!current) {
              const deleted = await this.#deleteAiItem(row, name);
              if (!deleted) {
                this.#deferAiOperation(row);
                continue;
              }
            } else {
              let item: AiSearchItemState;
              if (current.ai_item_id === null) {
                item = await withAiSearchItems(
                  this.env.HISTORY_AI_SEARCH,
                  (items) => withAiSearchResult(
                    items.upload(
                      name,
                      `${current.field === "user" ? "User" : "Assistant"}: ${current.content}`,
                      { metadata: {
                        organization_id: this.#organizationId(), team_id: current.team_id,
                        segment_id: row.segment_id,
                      } },
                    ),
                    copyAiSearchItemState,
                  ),
                );
                this.ctx.storage.sql.exec(
                  "UPDATE memory_segments SET ai_item_id = ? WHERE segment_id = ?",
                  item.id,
                  row.segment_id,
                );
              } else {
                try {
                  item = await withAiSearchItems(
                    this.env.HISTORY_AI_SEARCH,
                    (items) => withAiSearchItem(
                      items,
                      current.ai_item_id!,
                      (currentItem) => withAiSearchResult(
                        currentItem.info(),
                        copyAiSearchItemState,
                      ),
                    ),
                  );
                } catch (error) {
                  if (!isAiSearchNotFound(error)) throw error;
                  this.ctx.storage.sql.exec(
                    "UPDATE memory_segments SET ai_item_id = NULL WHERE segment_id = ?",
                    row.segment_id,
                  );
                  this.#deferAiOperation(row);
                  continue;
                }
              }
              if (item.status !== "completed") {
                if (item.status === "error" || item.status === "skipped" || item.status === "outdated") {
                  await withAiSearchItems(
                    this.env.HISTORY_AI_SEARCH,
                    (items) => withAiSearchItem(
                      items,
                      item.id,
                      (currentItem) => withAiSearchResult(
                        currentItem.sync(),
                        () => undefined,
                      ),
                    ),
                  );
                }
                this.#deferAiOperation(row);
                continue;
              }
            }
          }
          this.ctx.storage.sql.exec(
            "DELETE FROM memory_ai_outbox WHERE operation_id = ?",
            row.operation_id,
          );
        } catch (error) {
          this.#deferAiOperation(row);
          console.warn({ type: "memory_scope.ai_outbox_operation_failed", error_kind: errorKind(error) });
        }
      }
    }
    await this.#scheduleNextAlarm();
  }

  async #deleteAiItem(row: AiOutboxRow, key = `${row.segment_id}.md`): Promise<boolean> {
    // Built-in item keys are unique within their source. Exact key filtering
    // avoids treating colon-bearing segment filenames as search patterns.
    const ids = await withAiSearchItems(
      this.env.HISTORY_AI_SEARCH!,
      (items) => withAiSearchResult(
        items.list({
          key,
          source: "builtin",
          per_page: 50,
        } as AiSearchListItemsParams & { key: string }),
        (listed) => new Set(
          listed.result.filter((item) => item.key === key).map((item) => item.id),
        ),
      ),
    );
    if (row.ai_item_id !== null) ids.add(row.ai_item_id);
    if (ids.size === 0) {
      // An upload may have succeeded remotely before its ID was committed.
      // Require several empty reconciliations before retiring that tombstone.
      return row.attempt_count >= 3;
    }
    await withAiSearchItems(this.env.HISTORY_AI_SEARCH!, async (items) => {
      await Promise.all([...ids].map(async (id) => {
        try {
          await items.delete(id);
        } catch (error) {
          if (!isAiSearchNotFound(error)) throw error;
        }
      }));
    });
    this.ctx.storage.sql.exec(
      "UPDATE memory_ai_outbox SET ai_item_id = NULL WHERE operation_id = ?",
      row.operation_id,
    );
    return false;
  }

  #deferAiOperation(row: AiOutboxRow): void {
    const attempt = row.attempt_count + 1;
    this.ctx.storage.sql.exec(
      `UPDATE memory_ai_outbox SET attempt_count = ?, retry_at = ?
       WHERE operation_id = ?`,
      attempt,
      Date.now() + retryDelayMs(attempt),
      row.operation_id,
    );
  }

  async #scheduleNextAlarm(): Promise<void> {
    const row = this.ctx.storage.sql.exec<{ retry_at: number }>(
      "SELECT retry_at FROM memory_ai_outbox ORDER BY retry_at LIMIT 1",
    ).toArray()[0];
    if (!row) {
      await this.ctx.storage.deleteAlarm();
      return;
    }
    await this.ctx.storage.setAlarm(Math.max(Date.now() + 1, row.retry_at));
  }
}

function memoryHit(row: RankedMemoryTurnRow, query: string): HistorySearchHit {
  return {
    thread_id: row.thread_id,
    title: row.title,
    turn_id: row.turn_id,
    cursor: row.source_cursor,
    score: row.semantic_score ?? normalizedScore(row.rank),
    snippet: snippet(row.content, query),
  };
}

function interleaveRankedRows(
  primary: readonly RankedMemoryTurnRow[],
  secondary: readonly RankedMemoryTurnRow[],
  limit: number,
): RankedMemoryTurnRow[] {
  const rows: RankedMemoryTurnRow[] = [];
  const seen = new Set<string>();
  const append = (row: RankedMemoryTurnRow | undefined) => {
    if (row === undefined || seen.has(row.segment_id) || rows.length >= limit) return;
    seen.add(row.segment_id);
    rows.push(row);
  };
  for (let index = 0; rows.length < limit
    && (index < primary.length || index < secondary.length); index += 1) {
    append(primary[index]);
    append(secondary[index]);
  }
  return rows;
}

function normalizedScore(rank: number): number {
  if (rank <= -1) return Math.min(1, Math.max(0, -rank));
  if (rank < 0) return Math.min(1, Math.max(0, -rank / (1 - rank)));
  return 0;
}

function snippet(content: string, query: string): string {
  const compact = content.replace(/\s+/g, " ").trim();
  if (compact.length <= 360) return compact;
  const lowered = compact.toLocaleLowerCase();
  const match = historySearchTerms(query).reduce((earliest, term) => {
    const index = lowered.indexOf(term);
    return index < 0 ? earliest : Math.min(earliest, index);
  }, Number.POSITIVE_INFINITY);
  const start = Number.isFinite(match) ? Math.max(0, match - 120) : 0;
  const end = Math.min(compact.length, start + 358);
  return `${start > 0 ? "…" : ""}${compact.slice(start, end).trim()}${end < compact.length ? "…" : ""}`;
}

function memoryRecord(row: DurableMemoryRow, storage: DurableObjectStorage, content = readMemoryContent(storage, row)): MemoryRecord {
  return {
    key: { id: row.id, version: row.version },
    content,
    created_at_ms: row.created_at_ms,
    updated_at_ms: row.updated_at_ms,
    last_scanned_at_ms: row.last_scanned_at_ms,
    scan_count: row.scan_count,
    last_used_at_ms: row.last_used_at_ms,
    use_count: row.use_count,
    probation_until_ms: row.probation_until_ms,
  };
}

const SECRET_PREFIXES = [
  "sk-", "sk_", "ghp_", "gho_", "ghu_", "ghs_", "github_pat_",
  "xoxb-", "xoxp-", "xoxa-", "xoxr-", "akia",
] as const;
const SECRET_ASSIGNMENT_NAMES = [
  "password", "passwd", "secret", "token", "private_key", "private-key",
  "api_key", "api-key", "apikey",
] as const;

/** Conservative parity with Tact's persistence-boundary secret detector. */
function containsLikelySecret(content: string): boolean {
  const lower = content.toLowerCase();
  if (/(?:^|[^A-Za-z0-9_-])ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}(?=$|[^A-Za-z0-9_-])/u
    .test(content)) {
    return true;
  }
  if (lower.split(/\r?\n/u).some((line) => {
    const trimmed = line.trim();
    return (trimmed.startsWith("-----begin ") && trimmed.endsWith("private key-----"))
      || trimmed.startsWith("authorization:")
      || trimmed.startsWith("authorization=")
      || [...trimmed.matchAll(/bearer ([a-z0-9._~+/-]{12,128})/giu)]
        .some((match) => /[^a-z]/iu.test(match[1]!));
  })) return true;
  if (content.split(/\s+/u).some((word) => {
    const match = word.match(/^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)(?:[/?#]|$)/iu);
    if (!match) return false;
    const at = match[1]!.lastIndexOf("@");
    if (at < 0) return false;
    const credentials = match[1]!.slice(0, at);
    const separator = credentials.indexOf(":");
    return separator > 0 && separator < credentials.length - 1;
  })) return true;
  for (const prefix of SECRET_PREFIXES) {
    let offset = 0;
    while ((offset = lower.indexOf(prefix, offset)) >= 0) {
      const previous = offset === 0 ? "" : lower[offset - 1]!;
      const suffix = lower.slice(offset + prefix.length).match(/^[a-z0-9_-]{12,16}/u)?.[0];
      if ((!previous || !/[a-z0-9_]/u.test(previous)) && (suffix?.length ?? 0) >= 12) return true;
      offset += prefix.length;
    }
  }
  if (content.split(/\s+/u).some((word) => {
    const candidate = word.replace(/^[^A-Za-z0-9_.-]+|[^A-Za-z0-9_.-]+$/gu, "");
    const segments = candidate.split(".");
    return segments.length === 3 && segments[0]!.startsWith("eyJ")
      && segments.every((segment) => segment.length >= 8 && /^[A-Za-z0-9_-]+$/u.test(segment));
  })) return true;
  return SECRET_ASSIGNMENT_NAMES.some((name) => {
    let offset = 0;
    while ((offset = lower.indexOf(name, offset)) >= 0) {
      const remainder = lower.slice(offset + name.length).trimStart();
      if ((remainder.startsWith("=") || remainder.startsWith(":"))
        && remainder.slice(1).trimStart().length > 0) return true;
      offset += name.length;
    }
    return false;
  });
}

function retryDelayMs(attempt: number): number {
  return Math.min(MAX_AI_RETRY_DELAY_MS, 1_000 * (2 ** Math.max(0, attempt - 1)));
}

async function parseJsonBody<Value>(request: Request): Promise<Value> {
  try {
    return await request.json() as Value;
  } catch {
    throw new HistorySearchError(400, "invalid_json", "request body must be JSON");
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function errorKind(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}

function isAiSearchNotFound(error: unknown): boolean {
  return error instanceof Error && /not.?found/iu.test(`${error.name} ${error.message}`);
}
