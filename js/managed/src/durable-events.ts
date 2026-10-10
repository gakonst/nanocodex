const REPLAY_PAGE_SIZE = 256;
export const MAX_HISTORY_PAGE_SIZE = 256;
export const MAX_HISTORY_PAGE_BYTES = 4 * 1024 * 1024;
const KEEPALIVE_MS = 15_000;
const MAX_SUBSCRIBERS = 32;
// Link holders must not consume all owner event-stream slots.
const MAX_SHARED_SUBSCRIBERS = 16;
const MAX_CURSOR = 9_223_372_036_854_775_807n;
const DIRECT_EVENT_BYTES = 1_000_000;
const EVENT_CHUNK_CODE_UNITS = 256_000;
const sseEncoder = new TextEncoder();

export type ManagedEventRow = {
  cursor: string;
  created_at: number;
  message_json: string;
  turn_id: string | null;
};

export type DurableEvent<Message> = Readonly<{
  cursor: string;
  created_at: number;
  message: Message;
  turn_id: string | null;
}>;

export type DurableEventHistory<Message> = Readonly<{
  data: DurableEvent<Message>[];
  has_more: boolean;
  latest_cursor: string;
}>;

/**
 * Opt-in bounds for model-facing history readers. maxBytes replaces the page
 * byte budget. An event whose stored message exceeds maxEventBytes is never
 * hydrated: it is returned as a TruncatedEventMessage stand-in that keeps its
 * cursor, so cursor pagination neither skips nor stalls on it.
 */
export type HistoryBounds = Readonly<{ maxBytes?: number; maxEventBytes?: number }>;
export type TruncatedEventMessage = { truncated: true; message_bytes: number; preview: string };
/** Code points of the stored message JSON kept in a truncated stand-in. */
export const TRUNCATED_EVENT_PREVIEW_CHARS = 2_048;
const TRUNCATED_EVENT_COST = TRUNCATED_EVENT_PREVIEW_CHARS * 4 + 256;

export function truncatedEventMessage(messageJson: string, bytes: number): TruncatedEventMessage {
  let preview = messageJson.slice(0, TRUNCATED_EVENT_PREVIEW_CHARS);
  if (/[\uD800-\uDBFF]$/.test(preview)) preview = preview.slice(0, -1);
  return { truncated: true, message_bytes: bytes, preview };
}

/** Whether a stored message of this many bytes is returned as a stand-in. */
export function isOversizedEvent(bytes: number, bounds: HistoryBounds | undefined): boolean {
  return bounds?.maxEventBytes !== undefined && bytes > bounds.maxEventBytes;
}

/** Budgeted cost of one event on a bounded page. */
export function boundedEventCost(bytes: number, bounds: HistoryBounds | undefined): number {
  return isOversizedEvent(bytes, bounds) ? Math.min(bytes, TRUNCATED_EVENT_COST) : bytes;
}

export type DurableEventTail<Message> = Readonly<{
  events: readonly DurableEvent<Message>[];
  high_water_cursor: string;
}>;

type Subscriber = {
  abort: () => void;
  after: string;
  closed: boolean;
  dirty: boolean;
  keepalive?: ReturnType<typeof setInterval>;
  running: boolean;
  authorize?: () => boolean;
  project?: (event: DurableEvent<{ type: string }>) => DurableEvent<{ type: string }> | null;
  stopAfter?: (event: DurableEvent<{ type: string }>) => boolean;
  tag?: string;
  removeAbortListener?: () => void;
  page: (after: string, limit: number) => Promise<DurableEvent<{ type: string }>[]>;
  tail: Promise<void>;
  writer: WritableStreamDefaultWriter<Uint8Array>;
};

/**
 * Durable, cursor-addressed event projection for one Durable Object.
 *
 * SQLite is authoritative. Publication only wakes subscribers, which always
 * catch up from their last written cursor. That makes replay-to-live delivery
 * insensitive to notification loss, duplication, or interleaving.
 */
export class DurableEventLog<Message extends { type: string }> {
  readonly #storage: DurableObjectStorage;
  readonly #subscribers = new Set<Subscriber>();
  #staged: { message: Message; turnId: string | null; key: string; bytes: number } | undefined;
  #materialized: { event: DurableEvent<Message>; follower: string }[] = [];

  constructor(storage: DurableObjectStorage, readonly onAppend?: (event: DurableEvent<{ type: string }>) => void) {
    this.#storage = storage;
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_events (
        cursor INTEGER PRIMARY KEY AUTOINCREMENT,
        turn_id TEXT,
        message_json TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS managed_event_chunks (
        cursor INTEGER NOT NULL,
        chunk_index INTEGER NOT NULL,
        message_json TEXT NOT NULL,
        PRIMARY KEY (cursor, chunk_index),
        FOREIGN KEY (cursor) REFERENCES managed_events(cursor)
      );
      CREATE TABLE IF NOT EXISTS managed_event_meta (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0)
      );
      INSERT OR IGNORE INTO managed_event_meta (singleton, total_bytes)
      SELECT 1,
             (SELECT COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
              FROM managed_events)
               + (SELECT COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
                  FROM managed_event_chunks);
      UPDATE managed_event_meta
      SET total_bytes =
        (SELECT COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
         FROM managed_events)
          + (SELECT COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
             FROM managed_event_chunks)
      WHERE total_bytes = 0 AND EXISTS (SELECT 1 FROM managed_events);
    `);
  }

  /** Appends inside the caller's current SQLite transaction, if any.
   * A staged coalescable message is first materialized at the preceding
   * cursor in that same transaction, so durable order equals arrival order. */
  append(
    message: Message,
    turnId: string | null = null,
  ): DurableEvent<Message> {
    const staged = this.#staged;
    if (staged) {
      this.#staged = undefined;
      const materialized = this.#insert(staged.message, staged.turnId);
      const event = this.#insert(message, turnId);
      // Broadcast the materialized row only together with its follower, which
      // committed or rolled back in the same transaction.
      this.#materialized.push({ event: materialized, follower: event.cursor });
      return event;
    }
    return this.#insert(message, turnId);
  }

  /**
   * Holds one live-stream message (such as a text delta) in memory so that
   * consecutive messages with the same key share a single durable row. The
   * staged row is written by the next append, by flushStaged(), or merged with
   * the next compatible stage(). Returns an event that was flushed because the
   * staged key changed or the merged row would exceed maxBytes; the caller
   * publishes it. Staged messages carry no cursor until written.
   */
  stage(
    message: Message,
    turnId: string | null,
    key: string,
    merge: (staged: Message, next: Message) => Message | undefined,
    bytes: number,
    maxBytes: number,
  ): DurableEvent<Message> | undefined {
    const staged = this.#staged;
    if (staged && staged.key === key && staged.turnId === turnId && staged.bytes + bytes <= maxBytes) {
      const merged = merge(staged.message, message);
      if (merged !== undefined) {
        staged.message = merged;
        staged.bytes += bytes;
        return;
      }
    }
    const flushed = staged ? this.flushStaged() : undefined;
    this.#staged = { message, turnId, key, bytes };
    return flushed;
  }

  hasStaged(): boolean {
    return this.#staged !== undefined;
  }

  /** Writes the staged message in its own transaction; the caller publishes it. */
  flushStaged(): DurableEvent<Message> | undefined {
    const staged = this.#staged;
    if (!staged) return;
    this.#staged = undefined;
    return this.#storage.transactionSync(() => this.#insert(staged.message, staged.turnId));
  }

  /** Discards a staged message that must not become durable (deletion/fencing). */
  discardStaged(): void {
    this.#staged = undefined;
    this.#materialized = [];
  }

  /**
   * Materialized rows written in the same transaction as `published`, which
   * must be broadcast before it. Rows whose follower was never published (its
   * transaction failed, or its caller does not broadcast) are dropped from the
   * live queue; committed rows remain readable through history and SSE.
   */
  takeMaterialized(published: DurableEvent<Message>): DurableEvent<Message>[] {
    if (this.#materialized.length === 0) return [];
    const ready: DurableEvent<Message>[] = [];
    const pending: { event: DurableEvent<Message>; follower: string }[] = [];
    for (const entry of this.#materialized) {
      if (entry.follower === published.cursor) ready.push(entry.event);
      else if (compareCursor(entry.follower, published.cursor) > 0) pending.push(entry);
    }
    this.#materialized = pending;
    return ready;
  }

  #insert(
    message: Message,
    turnId: string | null,
  ): DurableEvent<Message> {
    const messageJson = JSON.stringify(message);
    const messageBytes = sseEncoder.encode(messageJson).byteLength;
    const createdAt = Date.now();
    const chunked = messageBytes > DIRECT_EVENT_BYTES;
    const inserted = this.#storage.sql.exec<{ cursor: string }>(
      `INSERT INTO managed_events (turn_id, message_json, created_at)
       VALUES (?, ?, ?)
       RETURNING CAST(cursor AS TEXT) AS cursor`,
      turnId,
      chunked ? "" : messageJson,
      createdAt,
    ).toArray()[0];
    if (!inserted || parseCursor(inserted.cursor) !== inserted.cursor || inserted.cursor === "0") {
      throw new Error("failed to allocate a durable event cursor");
    }
    if (chunked) {
      const chunks = messageChunks(messageJson);
      for (let index = 0; index < chunks.length; index += 1) {
        this.#storage.sql.exec(
          `INSERT INTO managed_event_chunks (cursor, chunk_index, message_json)
           VALUES (CAST(? AS INTEGER), ?, ?)`,
          inserted.cursor,
          index,
          chunks[index],
        );
      }
    }
    this.#storage.sql.exec(
      "UPDATE managed_event_meta SET total_bytes = total_bytes + ? WHERE singleton = 1",
      messageBytes,
    );
    const event = { cursor: inserted.cursor, created_at: createdAt, message, turn_id: turnId };
    this.onAppend?.(event);
    return event;
  }

  record(message: Message, turnId: string | null = null): DurableEvent<Message> {
    const event = this.#storage.transactionSync(() => this.append(message, turnId));
    this.publish(event);
    return event;
  }

  /** Signals that a committed row is available; subscribers reread SQLite. */
  publish(_event?: DurableEvent<Message>): void {
    for (const subscriber of this.#subscribers) this.#wake(subscriber);
  }

  latestCursor(): string {
    return this.#storage.sql.exec<{ cursor: string }>(
      "SELECT CAST(COALESCE(MAX(cursor), 0) AS TEXT) AS cursor FROM managed_events",
    ).toArray()[0]?.cursor ?? "0";
  }

  portableTail(after: string): DurableEventTail<Message> {
    const events = this.page(after, REPLAY_PAGE_SIZE);
    const sequence = this.#storage.sql.exec<{ cursor: string }>(
      `SELECT CAST(COALESCE((
         SELECT seq FROM sqlite_sequence WHERE name = 'managed_events'
       ), (SELECT MAX(cursor) FROM managed_events), 0) AS TEXT) AS cursor`,
    ).toArray()[0]?.cursor ?? "0";
    return { events, high_water_cursor: sequence };
  }

  adoptTail(tail: DurableEventTail<Message>, withinTransaction = true): void {
    const highWater = parseCursor(tail.high_water_cursor);
    if (highWater === undefined || !Array.isArray(tail.events)) {
      throw new Error("managed event portable tail is invalid");
    }
    let previous = "0";
    let totalBytes = 0;
    const encoded = tail.events.map((event) => {
      if (!event || typeof event !== "object"
        || typeof event.cursor !== "string" || parseCursor(event.cursor) !== event.cursor
        || event.cursor === "0" || compareCursor(previous, event.cursor) >= 0
        || compareCursor(event.cursor, highWater) > 0
        || !Number.isSafeInteger(event.created_at) || event.created_at < 0
        || (event.turn_id !== null && typeof event.turn_id !== "string")
        || !event.message || typeof event.message !== "object"
        || typeof event.message.type !== "string") {
        throw new Error("managed event portable tail contains an invalid event");
      }
      previous = event.cursor;
      const messageJson = JSON.stringify(event.message);
      const bytes = sseEncoder.encode(messageJson).byteLength;
      totalBytes += bytes;
      return { event, messageJson, bytes };
    });
    const adopt = () => {
      this.clear();
      for (const { event, messageJson, bytes } of encoded) {
        const chunked = bytes > DIRECT_EVENT_BYTES;
        this.#storage.sql.exec(
          `INSERT INTO managed_events (cursor, turn_id, message_json, created_at)
           VALUES (CAST(? AS INTEGER), ?, ?, ?)`,
          event.cursor,
          event.turn_id,
          chunked ? "" : messageJson,
          event.created_at,
        );
        if (chunked) {
          for (const [chunkIndex, chunk] of messageChunks(messageJson).entries()) {
            this.#storage.sql.exec(
              `INSERT INTO managed_event_chunks (cursor, chunk_index, message_json)
               VALUES (CAST(? AS INTEGER), ?, ?)`,
              event.cursor,
              chunkIndex,
              chunk,
            );
          }
        }
      }
      this.#storage.sql.exec(
        "UPDATE managed_event_meta SET total_bytes = ? WHERE singleton = 1",
        totalBytes,
      );
      this.#storage.sql.exec("DELETE FROM sqlite_sequence WHERE name = 'managed_events'");
      this.#storage.sql.exec(
        "INSERT INTO sqlite_sequence (name, seq) VALUES ('managed_events', CAST(? AS INTEGER))",
        highWater,
      );
    };
    if (withinTransaction) this.#storage.transactionSync(adopt);
    else adopt();
  }

  totalBytes(): number {
    return this.#storage.sql.exec<{ total_bytes: number }>(
      "SELECT total_bytes FROM managed_event_meta WHERE singleton = 1",
    ).toArray()[0]?.total_bytes ?? 0;
  }

  page(after: string, limit = REPLAY_PAGE_SIZE, bounds?: HistoryBounds): DurableEvent<Message>[] {
    return this.#readPage(after, limit, false, bounds).data;
  }

  /** Reads a bounded payload window in chronological presentation order. */
  history(before: string | undefined, limit: number, bounds?: HistoryBounds): DurableEventHistory<Message> {
    return this.#readPage(before, limit, true, bounds);
  }

  #readPage(
    cursor: string | undefined,
    limit: number,
    newestFirst: boolean,
    bounds?: HistoryBounds,
  ): DurableEventHistory<Message> {
    const direction = newestFirst ? "DESC" : "ASC";
    const boundary = cursor === undefined ? "" : `WHERE events.cursor ${newestFirst ? "<" : ">"} CAST(? AS INTEGER)`;
    // Select sizes before crossing the SQLite/JS boundary. A row-count limit
    // alone can hydrate hundreds of megabytes of chunked tool/image payloads.
    const candidates = this.#storage.sql.exec<{ cursor: string; bytes: number }>(
      `SELECT CAST(events.cursor AS TEXT) AS cursor,
              LENGTH(CAST(events.message_json AS BLOB)) + COALESCE((
                SELECT SUM(LENGTH(CAST(chunks.message_json AS BLOB)))
                FROM managed_event_chunks chunks WHERE chunks.cursor = events.cursor
              ), 0) AS bytes
       FROM managed_events events ${boundary}
       ORDER BY events.cursor ${direction} LIMIT ?`,
      ...(cursor === undefined ? [] : [cursor]), limit + 1,
    ).toArray();
    const budget = bounds?.maxBytes ?? MAX_HISTORY_PAGE_BYTES;
    let count = 0, bytes = 0;
    for (const candidate of candidates) {
      const cost = boundedEventCost(candidate.bytes, bounds);
      if (count >= limit || (count > 0 && bytes + cost > budget)) break;
      count++; bytes += cost;
    }
    const selected = candidates.slice(0, count);
    const oversized = new Map(selected.filter(({ bytes }) => isOversizedEvent(bytes, bounds))
      .map(({ cursor, bytes }) => [cursor, bytes]));
    const first = selected[0]?.cursor, last = selected.at(-1)?.cursor;
    // Oversized direct payloads stay inside SQLite; their chunks are never read.
    // For a direct row its stored length is its whole size, so this predicate
    // blanks exactly the oversized direct rows; chunked rows are already ''.
    const rows = first === undefined || last === undefined ? [] : this.#storage.sql.exec<ManagedEventRow>(
      `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, created_at,
              CASE WHEN ? IS NOT NULL AND LENGTH(CAST(message_json AS BLOB)) > ? THEN '' ELSE message_json END AS message_json
       FROM managed_events WHERE cursor >= CAST(? AS INTEGER) AND cursor <= CAST(? AS INTEGER)
       ORDER BY managed_events.cursor`,
      oversized.size > 0 ? 1 : null, bounds?.maxEventBytes ?? 0,
      newestFirst ? last : first, newestFirst ? first : last,
    ).toArray();
    const hydrated = new Map(hydrateManagedEventRows(this.#storage, rows.filter(({ cursor }) => !oversized.has(cursor)))
      .map((row) => [row.cursor, row]));
    return {
      data: rows.map((row) => ({
        cursor: row.cursor, created_at: row.created_at, turn_id: row.turn_id,
        message: oversized.has(row.cursor)
          ? truncatedEventMessage(this.#messagePrefix(row.cursor), oversized.get(row.cursor)!) as unknown as Message
          : JSON.parse(hydrated.get(row.cursor)!.message_json) as Message,
      })),
      has_more: candidates.length > count,
      latest_cursor: this.latestCursor(),
    };
  }

  /** Leading code points of a stored message without hydrating its chunks. */
  #messagePrefix(cursor: string): string {
    return this.#storage.sql.exec<{ preview: string }>(
      `SELECT SUBSTR(CASE WHEN events.message_json = '' THEN COALESCE((
                SELECT chunks.message_json FROM managed_event_chunks chunks
                WHERE chunks.cursor = events.cursor AND chunks.chunk_index = 0
              ), '') ELSE events.message_json END, 1, ?) AS preview
       FROM managed_events events WHERE events.cursor = CAST(? AS INTEGER)`,
      TRUNCATED_EVENT_PREVIEW_CHARS, cursor,
    ).toArray()[0]?.preview ?? "";
  }

  stream(after: string, signal?: AbortSignal): Response {
    return this.streamWithPage(
      after,
      this.latestCursor(),
      async (cursor, limit) => this.page(cursor, limit),
      signal,
    );
  }

  streamWithPage(
    after: string,
    latest: string,
    page: (after: string, limit: number) => Promise<DurableEvent<Message>[]>,
    signal?: AbortSignal,
    options?: {
      authorize?: () => boolean;
      project?: (event: DurableEvent<Message>) => DurableEvent<{ type: string }> | null;
      tag?: string;
      /** Close only after this durable event has been delivered to the reader. */
      stopAfter?: (event: DurableEvent<Message>) => boolean;
    },
  ): Response {
    const cursor = parseCursor(after);
    if (cursor === undefined) {
      return Response.json({ error: "invalid_cursor" }, { status: 400 });
    }
    if (compareCursor(cursor, latest) > 0) {
      return Response.json(
        { error: "cursor_ahead", latest_cursor: latest },
        { status: 409, headers: { "cache-control": "no-store" } },
      );
    }
    if (options?.authorize && !options.authorize()) {
      return Response.json({ error: "not_found" }, { status: 404, headers: { "cache-control": "no-store" } });
    }
    const sharedLimitReached = options?.tag
      && [...this.#subscribers].filter((subscriber) => subscriber.tag).length >= MAX_SHARED_SUBSCRIBERS;
    if (this.#subscribers.size >= MAX_SUBSCRIBERS || sharedLimitReached) {
      return Response.json(
        { error: "event_stream_limit", limit: sharedLimitReached ? MAX_SHARED_SUBSCRIBERS : MAX_SUBSCRIBERS },
        {
          status: 429,
          headers: { "cache-control": "no-store", "retry-after": "1" },
        },
      );
    }

    let controller!: TransformStreamDefaultController<Uint8Array>;
    const body = new TransformStream<Uint8Array, Uint8Array>({
      start(value) { controller = value; },
    });
    const subscriber: Subscriber = {
      abort: () => controller.error(new Error("Event stream canceled")),
      after: cursor,
      closed: false,
      dirty: false,
      page: page as Subscriber["page"],
      running: false,
      authorize: options?.authorize,
      project: options?.project as Subscriber["project"],
      tag: options?.tag,
      stopAfter: options?.stopAfter as Subscriber["stopAfter"],
      tail: Promise.resolve(),
      writer: body.writable.getWriter(),
    };
    this.#subscribers.add(subscriber);
    subscriber.tail = subscriber.writer.write(
      sseEncoder.encode(`retry: 1000\n: cursor ${cursor}\n\n`),
    );
    this.#wake(subscriber);
    subscriber.keepalive = setInterval(() => {
      // A replaced DO instance may still write comments after its storage has
      // disconnected. Recheck the durable cursor before advertising liveness:
      // failed reads close the stream so clients reconnect to the active owner.
      // This also catches committed events whose publication wakeup was lost.
      this.#wake(subscriber);
      this.#enqueueComment(subscriber, sseEncoder.encode(": keepalive\n\n"));
    }, KEEPALIVE_MS);
    const close = () => this.#close(subscriber);
    const abort = () => this.#close(subscriber, true);
    subscriber.removeAbortListener = () => signal?.removeEventListener("abort", abort);
    signal?.addEventListener("abort", abort, { once: true });
    void subscriber.writer.closed.then(close, close);
    void subscriber.tail.catch(close);
    if (signal?.aborted) abort();

    return new Response(body.readable, {
      headers: {
        "cache-control": "no-cache, no-store",
        connection: "keep-alive",
        "content-type": "text/event-stream; charset=utf-8",
        "x-accel-buffering": "no",
      },
    });
  }

  /** Terminate streams bound to a revoked share link without disturbing owner streams. */
  closeTagged(tag: string): void {
    for (const subscriber of this.#subscribers) {
      if (subscriber.tag === tag) this.#close(subscriber, true);
    }
  }

  clear(): void {
    this.discardStaged();
    for (const subscriber of this.#subscribers) this.#close(subscriber, true);
    this.#storage.sql.exec("DELETE FROM managed_event_chunks");
    this.#storage.sql.exec("DELETE FROM managed_events");
    this.#storage.sql.exec(
      "UPDATE managed_event_meta SET total_bytes = 0 WHERE singleton = 1",
    );
  }

  #wake(subscriber: Subscriber): void {
    if (subscriber.closed) return;
    subscriber.dirty = true;
    if (subscriber.running) return;
    subscriber.running = true;
    subscriber.tail = subscriber.tail.then(async () => {
      try {
        while (!subscriber.closed && subscriber.dirty) {
          subscriber.dirty = false;
          await this.#catchUp(subscriber);
        }
      } finally {
        subscriber.running = false;
        if (subscriber.dirty && !subscriber.closed) this.#wake(subscriber);
      }
    });
    void subscriber.tail.catch(() => this.#close(subscriber));
  }

  async #catchUp(subscriber: Subscriber): Promise<void> {
    while (!subscriber.closed) {
      if (subscriber.authorize && !subscriber.authorize()) return this.#close(subscriber, true);
      const events = await subscriber.page(subscriber.after, REPLAY_PAGE_SIZE);
      if (subscriber.closed) return;
      if (subscriber.authorize && !subscriber.authorize()) return this.#close(subscriber, true);
      if (events.length === 0) return;
      for (const event of events) {
        if (subscriber.authorize && !subscriber.authorize()) return this.#close(subscriber, true);
        const projected = subscriber.project ? subscriber.project(event) : event;
        if (projected) await subscriber.writer.write(encodeEvent(projected));
        subscriber.after = event.cursor;
        if (subscriber.stopAfter?.(event)) return this.#close(subscriber);
        if (subscriber.closed) return;
      }
      // Byte limits and archive boundaries can produce short nonterminal
      // pages. Only an empty page proves that this subscriber is caught up.
    }
  }

  #enqueueComment(subscriber: Subscriber, encoded: Uint8Array): void {
    if (subscriber.closed) return;
    if (subscriber.authorize && !subscriber.authorize()) return this.#close(subscriber, true);
    subscriber.tail = subscriber.tail.then(() => subscriber.writer.write(encoded));
    void subscriber.tail.catch(() => this.#close(subscriber));
  }

  #close(subscriber: Subscriber, cancelled = false): void {
    if (subscriber.closed) return;
    subscriber.closed = true;
    if (subscriber.keepalive !== undefined) clearInterval(subscriber.keepalive);
    this.#subscribers.delete(subscriber);
    subscriber.removeAbortListener?.();
    subscriber.removeAbortListener = undefined;
    // close() waits behind a backpressured write, retaining its history page
    // after the subscriber slot is freed. Cancellation must interrupt that write.
    if (cancelled) subscriber.abort();
    const finished = cancelled ? subscriber.writer.abort() : subscriber.writer.close();
    void finished.catch(() => {});
  }
}

/** Reassembles only the selected logical rows before callers parse their JSON. */
export function hydrateManagedEventRows(
  storage: DurableObjectStorage,
  rows: ManagedEventRow[],
): ManagedEventRow[] {
  if (rows.length === 0) return rows;
  const selected = [...new Set(rows.map(({ cursor }) => cursor))];
  const payloads = new Map<string, string[]>();
  for (let offset = 0; offset < selected.length; offset += 99) {
    const cursors = selected.slice(offset, offset + 99);
    const placeholders = cursors.map(() => "CAST(? AS INTEGER)").join(", ");
    const chunks = storage.sql.exec<{
      chunk_index: number;
      cursor: string;
      message_json: string;
    }>(
      `SELECT CAST(cursor AS TEXT) AS cursor, chunk_index, message_json
       FROM managed_event_chunks
       WHERE cursor IN (${placeholders})
       ORDER BY cursor, chunk_index`,
      ...cursors,
    ).toArray();
    for (const chunk of chunks) {
      const retained = payloads.get(chunk.cursor) ?? [];
      if (chunk.chunk_index !== retained.length || typeof chunk.message_json !== "string") {
        throw new Error(`invalid managed event chunks for cursor ${chunk.cursor}`);
      }
      retained.push(chunk.message_json);
      payloads.set(chunk.cursor, retained);
    }
  }
  return rows.map((row) => {
    const chunks = payloads.get(row.cursor);
    if (chunks === undefined) {
      if (row.message_json === "") {
        throw new Error(`missing managed event chunks for cursor ${row.cursor}`);
      }
      return row;
    }
    if (row.message_json !== "") {
      throw new Error(`invalid managed event chunk head for cursor ${row.cursor}`);
    }
    return { ...row, message_json: chunks.join("") };
  });
}

function messageChunks(message: string): string[] {
  const chunks: string[] = [];
  for (let offset = 0; offset < message.length;) {
    let end = Math.min(offset + EVENT_CHUNK_CODE_UNITS, message.length);
    if (end < message.length
      && isHighSurrogate(message.charCodeAt(end - 1))
      && isLowSurrogate(message.charCodeAt(end))) {
      end -= 1;
    }
    chunks.push(message.slice(offset, end));
    offset = end;
  }
  return chunks;
}

function isHighSurrogate(codeUnit: number): boolean {
  return codeUnit >= 0xd800 && codeUnit <= 0xdbff;
}

function isLowSurrogate(codeUnit: number): boolean {
  return codeUnit >= 0xdc00 && codeUnit <= 0xdfff;
}

export function parseCursor(value: string | null): string | undefined {
  if (value === null || value === "") return "0";
  if (!/^[0-9]+$/.test(value)) return undefined;
  try {
    const cursor = BigInt(value);
    return cursor <= MAX_CURSOR ? cursor.toString() : undefined;
  } catch {
    return undefined;
  }
}

function compareCursor(left: string, right: string): number {
  const leftCursor = BigInt(left);
  const rightCursor = BigInt(right);
  return leftCursor < rightCursor ? -1 : leftCursor > rightCursor ? 1 : 0;
}

function encodeEvent<Message extends { type: string }>(event: DurableEvent<Message>): Uint8Array {
  const type = safeEventName(event.message.type);
  const data = JSON.stringify({
    cursor: event.cursor,
    created_at: event.created_at,
    turn_id: event.turn_id,
    ...event.message,
  });
  return sseEncoder.encode(`id: ${event.cursor}\nevent: ${type}\ndata: ${data}\n\n`);
}

function safeEventName(value: string): string {
  const normalized = value.replace(/[^A-Za-z0-9_.-]/g, "_").slice(0, 128);
  return normalized || "message";
}
