const EVENT_TAG = "nanocodex-agent-events";
const MAX_CLIENTS = 64;
const MAX_EVENT_BYTES = 2 * 1024 * 1024;
const MAX_LOG_BYTES = 64 * 1024 * 1024;
const REPLAY_PAGE_EVENTS = 16;
const REPLAY_PAGE_BYTES = MAX_EVENT_BYTES + 1024;
const REPLAY_CONTROL_RESERVE_BYTES = 256;
const MAX_CURSOR = 9_223_372_036_854_775_807n;
const encoder = new TextEncoder();

/** Durable, read-only AgentEvent fan-out for one Cloudflare Durable Object. */
export function createCloudflareEventSocket(context) {
  validateContext(context);
  const storage = context.storage;
  initializeEventStorage(storage);
  recoverSockets(context);

  return Object.freeze({
    connect(request) {
      return connect(context, request);
    },
    publish(event) {
      record(storage, event);
      recoverSockets(context);
    },
    fail(error) {
      const detail = error instanceof Error ? error.message : String(error);
      storage.sql.exec(
        "UPDATE nanocodex_cloudflare_event_meta SET stream_error = ? WHERE singleton = 1",
        detail,
      );
      for (const socket of context.getWebSockets(EVENT_TAG)) {
        try { socket.close(1011, "Nanocodex event stream failed"); } catch { /* already closed */ }
      }
    },
  });
}

/** @internal Clears only the Cloudflare adapter's event projection. */
export function clearCloudflareEventSocket(context) {
  validateContext(context);
  const storage = context.storage;
  initializeEventStorage(storage);
  for (const socket of context.getWebSockets(EVENT_TAG)) {
    close(socket, 1000, "Nanocodex Agent destroyed");
  }
  storage.sql.exec("DELETE FROM nanocodex_cloudflare_events");
  storage.sql.exec(
    `UPDATE nanocodex_cloudflare_event_meta
     SET total_bytes = 0, stream_error = NULL
     WHERE singleton = 1`,
  );
}

function initializeEventStorage(storage) {
  storage.sql.exec(`
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_events (
      cursor INTEGER PRIMARY KEY AUTOINCREMENT,
      event_json TEXT NOT NULL,
      created_at INTEGER NOT NULL
    )
  `);
  storage.sql.exec(`
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_event_meta (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
      total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
      stream_error TEXT
    )
  `);
  storage.sql.exec(`
    INSERT OR IGNORE INTO nanocodex_cloudflare_event_meta
      (singleton, total_bytes, stream_error)
    SELECT 1, COALESCE(SUM(LENGTH(CAST(event_json AS BLOB))), 0), NULL
    FROM nanocodex_cloudflare_events
  `);
}

function validateContext(context) {
  if (!context || typeof context !== "object") {
    throw new TypeError("Cloudflare Agent requires a Durable Object context");
  }
  if (!context.storage?.sql || typeof context.storage.sql.exec !== "function"
    || typeof context.storage.transactionSync !== "function") {
    throw new TypeError("Cloudflare Agent requires Durable Object SQLite storage");
  }
  if (typeof context.acceptWebSocket !== "function" || typeof context.getWebSockets !== "function") {
    throw new TypeError("Cloudflare Agent requires hibernatable Durable Object WebSockets");
  }
}

function connect(context, request) {
  if (!(request instanceof Request)) {
    throw new TypeError("agent.events.connect requires a Request");
  }
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  const url = new URL(request.url);
  const values = url.searchParams.getAll("cursor");
  const cursor = parseCursor(values.length === 0 ? null : values.length === 1 ? values[0] : undefined);
  if (cursor === undefined) {
    return Response.json({ error: "invalid_cursor" }, { status: 400 });
  }
  const streamError = eventMeta(context.storage).stream_error;
  if (streamError !== null) {
    return Response.json(
      { error: "event_stream_failed" },
      { status: 503, headers: { "retry-after": "1" } },
    );
  }
  const latest = latestCursor(context.storage);
  if (BigInt(cursor) > BigInt(latest)) {
    return Response.json(
      { error: "cursor_ahead", latest_cursor: latest },
      { status: 409 },
    );
  }
  if (context.getWebSockets(EVENT_TAG).length >= MAX_CLIENTS) {
    return Response.json(
      { error: "event_socket_limit", limit: MAX_CLIENTS },
      { status: 429, headers: { "retry-after": "1" } },
    );
  }
  if (typeof globalThis.WebSocketPair !== "function") {
    throw new Error("Cloudflare WebSocketPair is unavailable in this runtime");
  }
  const pair = new globalThis.WebSocketPair();
  const client = pair[0];
  const server = pair[1];
  if (!client || !server || typeof server.serializeAttachment !== "function"
    || typeof server.deserializeAttachment !== "function") {
    throw new Error("Cloudflare WebSocketPair returned an invalid pair");
  }
  server.serializeAttachment({ version: 1, cursor });
  context.acceptWebSocket(server, [EVENT_TAG]);
  try {
    catchUp(context.storage, server);
  } catch (error) {
    try { server.close(1011, "Nanocodex event replay failed"); } catch { /* already closed */ }
    throw error;
  }
  return new Response(null, { status: 101, webSocket: client });
}

function record(storage, event) {
  const eventJson = JSON.stringify(event);
  const bytes = encoder.encode(eventJson).byteLength;
  if (bytes > MAX_EVENT_BYTES) {
    throw new RangeError(`Cloudflare AgentEvent exceeds ${MAX_EVENT_BYTES} encoded bytes`);
  }
  return storage.transactionSync(() => {
    const meta = eventMeta(storage);
    if (meta.stream_error !== null) throw new Error("Cloudflare Agent event stream has failed");
    if (meta.total_bytes + bytes > MAX_LOG_BYTES) {
      throw new RangeError(`Cloudflare AgentEvent log exceeds ${MAX_LOG_BYTES} encoded bytes`);
    }
    const inserted = storage.sql.exec(
      `INSERT INTO nanocodex_cloudflare_events (event_json, created_at)
       VALUES (?, ?)
       RETURNING CAST(cursor AS TEXT) AS cursor`,
      eventJson,
      Date.now(),
    ).toArray()[0];
    const cursor = parseCursor(inserted?.cursor);
    if (cursor === undefined || cursor === "0") {
      throw new Error("Cloudflare Agent failed to allocate an event cursor");
    }
    storage.sql.exec(
      "UPDATE nanocodex_cloudflare_event_meta SET total_bytes = total_bytes + ? WHERE singleton = 1",
      bytes,
    );
    return { cursor };
  });
}

function recoverSockets(context) {
  const sockets = context.getWebSockets(EVENT_TAG);
  if (eventMeta(context.storage).stream_error !== null) {
    for (const socket of sockets) close(socket, 1011, "Nanocodex event stream failed");
    return;
  }
  for (const socket of sockets) catchUp(context.storage, socket);
}

function catchUp(storage, socket) {
  if (socket.readyState !== 1) return;
  const attachment = socketAttachment(socket);
  if (attachment === undefined) {
    close(socket, 1011, "Nanocodex event replay state is invalid");
    return;
  }
  const latest = latestCursor(storage);
  if (BigInt(attachment.cursor) > BigInt(latest)) {
    close(socket, 1011, "Nanocodex event replay cursor is ahead");
    return;
  }
  if (attachment.replayPaused) {
    pauseReplay(socket, attachment.cursor, latest, 0);
    return;
  }

  let cursor = attachment.cursor;
  let pageBytes = 0;
  let pageEvents = 0;
  while (pageEvents < REPLAY_PAGE_EVENTS) {
    const row = nextEventRow(storage, cursor);
    if (row === undefined) return;
    const encoded = `{"cursor":${JSON.stringify(row.cursor)},"event":${row.event_json}}`;
    const bytes = encoder.encode(encoded).byteLength;
    if (bytes > REPLAY_PAGE_BYTES - REPLAY_CONTROL_RESERVE_BYTES) {
      close(socket, 1011, "Nanocodex event exceeds replay page budget");
      return;
    }
    if (pageEvents > 0
      && pageBytes + bytes > REPLAY_PAGE_BYTES - REPLAY_CONTROL_RESERVE_BYTES) {
      break;
    }
    if (!sendEvent(socket, encoded, row.cursor)) return;
    cursor = row.cursor;
    pageBytes += bytes;
    pageEvents += 1;
  }

  const replayLatest = latestCursor(storage);
  if (BigInt(cursor) < BigInt(replayLatest)) {
    pauseReplay(socket, cursor, replayLatest, pageBytes);
  }
}

function socketAttachment(socket) {
  if (typeof socket.deserializeAttachment !== "function") return undefined;
  let attachment;
  try {
    attachment = socket.deserializeAttachment();
  } catch {
    return undefined;
  }
  if (!attachment || attachment.version !== 1
    || (attachment.replayPaused !== undefined && typeof attachment.replayPaused !== "boolean")) {
    return undefined;
  }
  const cursor = parseCursor(attachment.cursor);
  return cursor === undefined
    ? undefined
    : { cursor, replayPaused: attachment.replayPaused === true };
}

function sendEvent(socket, encoded, cursor) {
  try {
    socket.send(encoded);
    socket.serializeAttachment({ version: 1, cursor });
    return true;
  } catch {
    close(socket, 1011, "Nanocodex event delivery failed");
    return false;
  }
}

function pauseReplay(socket, cursor, latest, pageBytes) {
  const encoded = JSON.stringify({
    type: "replay_paused",
    cursor,
    latest_cursor: latest,
  });
  if (pageBytes + encoder.encode(encoded).byteLength > REPLAY_PAGE_BYTES) {
    close(socket, 1011, "Nanocodex replay control exceeds page budget");
    return;
  }
  try {
    // The client must observe the fence before it can continue from its cursor.
    // Checkpointing first could strand a restored socket behind an unseen fence.
    socket.send(encoded);
    socket.serializeAttachment({ version: 1, cursor, replayPaused: true });
    socket.close(1013, "Nanocodex event replay continuation required");
  } catch {
    close(socket, 1011, "Nanocodex event replay pause failed");
  }
}

function close(socket, code, reason) {
  try { socket.close(code, reason); } catch { /* already closed */ }
}

function nextEventRow(storage, after) {
  return storage.sql.exec(
    `SELECT CAST(cursor AS TEXT) AS cursor, event_json
     FROM nanocodex_cloudflare_events
     WHERE cursor > CAST(? AS INTEGER)
     ORDER BY cursor
     LIMIT 1`,
    after,
  ).toArray()[0];
}

function latestCursor(storage) {
  return storage.sql.exec(
    "SELECT CAST(COALESCE(MAX(cursor), 0) AS TEXT) AS cursor FROM nanocodex_cloudflare_events",
  ).toArray()[0]?.cursor ?? "0";
}

function eventMeta(storage) {
  return storage.sql.exec(
    "SELECT total_bytes, stream_error FROM nanocodex_cloudflare_event_meta WHERE singleton = 1",
  ).toArray()[0] ?? { total_bytes: 0, stream_error: null };
}

function parseCursor(value) {
  if (value === null || value === "") return "0";
  if (typeof value !== "string" || !/^[0-9]+$/.test(value)) return undefined;
  try {
    const cursor = BigInt(value);
    return cursor <= MAX_CURSOR ? cursor.toString() : undefined;
  } catch {
    return undefined;
  }
}
