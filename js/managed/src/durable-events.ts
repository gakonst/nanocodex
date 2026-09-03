const REPLAY_PAGE_SIZE = 256;
export const MAX_HISTORY_PAGE_SIZE = 256;
const KEEPALIVE_MS = 15_000;
const MAX_SUBSCRIBERS = 32;
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

export type DurableEventTail<Message> = Readonly<{
  events: readonly DurableEvent<Message>[];
  high_water_cursor: string;
}>;

type Subscriber = {
  after: string;
  closed: boolean;
  dirty: boolean;
  keepalive?: ReturnType<typeof setInterval>;
  running: boolean;
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

  constructor(storage: DurableObjectStorage) {
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

  /** Appends inside the caller's current SQLite transaction, if any. */
  append(
    message: Message,
    turnId: string | null = null,
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
    return { cursor: inserted.cursor, created_at: createdAt, message, turn_id: turnId };
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

  page(after: string, limit = REPLAY_PAGE_SIZE): DurableEvent<Message>[] {
    const rows = this.#storage.sql.exec<ManagedEventRow>(
      `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, message_json, created_at
       FROM managed_events
       WHERE cursor > CAST(? AS INTEGER)
       ORDER BY managed_events.cursor
       LIMIT ?`,
      after,
      limit,
    ).toArray();
    return hydrateManagedEventRows(this.#storage, rows).map((row) => ({
      cursor: row.cursor,
      created_at: row.created_at,
      message: JSON.parse(row.message_json) as Message,
      turn_id: row.turn_id,
    }));
  }

  /** Returns a newest-first storage query in chronological presentation order. */
  history(before: string | undefined, limit: number): DurableEventHistory<Message> {
    const rows = (before === undefined
      ? this.#storage.sql.exec<ManagedEventRow>(
        `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, message_json, created_at
         FROM managed_events
         ORDER BY managed_events.cursor DESC
         LIMIT ?`,
        limit + 1,
      )
      : this.#storage.sql.exec<ManagedEventRow>(
        `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, message_json, created_at
         FROM managed_events
         WHERE cursor < CAST(? AS INTEGER)
         ORDER BY managed_events.cursor DESC
         LIMIT ?`,
        before,
        limit + 1,
      )).toArray();
    const hasMore = rows.length > limit;
    if (hasMore) rows.pop();
    return {
      data: hydrateManagedEventRows(this.#storage, rows).reverse().map((row) => ({
        cursor: row.cursor,
        created_at: row.created_at,
        message: JSON.parse(row.message_json) as Message,
        turn_id: row.turn_id,
      })),
      has_more: hasMore,
      latest_cursor: this.latestCursor(),
    };
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
    if (this.#subscribers.size >= MAX_SUBSCRIBERS) {
      return Response.json(
        { error: "event_stream_limit", limit: MAX_SUBSCRIBERS },
        {
          status: 429,
          headers: { "cache-control": "no-store", "retry-after": "1" },
        },
      );
    }

    const body = new TransformStream<Uint8Array, Uint8Array>();
    const subscriber: Subscriber = {
      after: cursor,
      closed: false,
      dirty: false,
      page: page as Subscriber["page"],
      running: false,
      tail: Promise.resolve(),
      writer: body.writable.getWriter(),
    };
    this.#subscribers.add(subscriber);
    subscriber.tail = subscriber.writer.write(
      sseEncoder.encode(`retry: 1000\n: cursor ${cursor}\n\n`),
    );
    this.#wake(subscriber);
    subscriber.keepalive = setInterval(() => {
      this.#enqueueComment(subscriber, sseEncoder.encode(": keepalive\n\n"));
    }, KEEPALIVE_MS);
    const close = () => this.#close(subscriber);
    signal?.addEventListener("abort", close, { once: true });
    void subscriber.writer.closed.then(close, close);
    void subscriber.tail.catch(close);
    if (signal?.aborted) close();

    return new Response(body.readable, {
      headers: {
        "cache-control": "no-cache, no-store",
        connection: "keep-alive",
        "content-type": "text/event-stream; charset=utf-8",
        "x-accel-buffering": "no",
      },
    });
  }

  clear(): void {
    for (const subscriber of this.#subscribers) this.#close(subscriber);
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
      const events = await subscriber.page(subscriber.after, REPLAY_PAGE_SIZE);
      if (events.length === 0) return;
      for (const event of events) {
        await subscriber.writer.write(encodeEvent(event));
        subscriber.after = event.cursor;
        if (subscriber.closed) return;
      }
      if (events.length < REPLAY_PAGE_SIZE) return;
    }
  }

  #enqueueComment(subscriber: Subscriber, encoded: Uint8Array): void {
    if (subscriber.closed) return;
    subscriber.tail = subscriber.tail.then(() => subscriber.writer.write(encoded));
    void subscriber.tail.catch(() => this.#close(subscriber));
  }

  #close(subscriber: Subscriber): void {
    if (subscriber.closed) return;
    subscriber.closed = true;
    if (subscriber.keepalive !== undefined) clearInterval(subscriber.keepalive);
    this.#subscribers.delete(subscriber);
    void subscriber.writer.close().catch(() => {});
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
