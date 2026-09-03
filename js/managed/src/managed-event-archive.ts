import {
  hydrateManagedEventRows,
  type DurableEvent,
  type DurableEventHistory,
  type DurableEventLog,
  type ManagedEventRow,
} from "./durable-events";
import { sha256Hex } from "./archive-hash";

const VERSION = 1;
const DEFAULT_SEAL_THRESHOLD_BYTES = 16 * 1024 * 1024;
const DEFAULT_SEGMENT_TARGET_BYTES = 8 * 1024 * 1024;
const DEFAULT_RECENT_EVENT_COUNT = 512;
const MAX_RECENT_DESCRIPTORS = 16;
const MAX_SEAL_ROWS = 4_096;
const encoder = new TextEncoder();

type EventRow = ManagedEventRow;

type EventIndexRow = Omit<EventRow, "message_json"> & {
  message_bytes: number;
};

export type ManagedEventArchiveState = Readonly<{
  archived_bytes: number;
  archived_events: number;
  archived_through: string;
  index_node_count: number;
  index_root_key: string | null;
  recent_json: string;
  segment_count: number;
}>;

type ArchiveStateRow = ManagedEventArchiveState;

type ArchiveReadFence = Readonly<{
  archived_events: number;
  archived_through: string;
}>;

type SegmentDescriptor = Readonly<{
  bytes: number;
  count: number;
  created_at: number;
  end_cursor: string;
  key: string;
  start_cursor: string;
}>;

type SegmentEnvelope<Message> = Readonly<{
  events: DurableEvent<Message>[];
  kind: "managed_event_segment";
  version: 1;
}>;

type IndexEnvelope = Readonly<{
  descriptors: SegmentDescriptor[];
  kind: "managed_event_index";
  ordinal: number;
  version: 1;
}>;

type SegmentReadCache<Message> = {
  segment?: { events: DurableEvent<Message>[]; key: string };
};

export type ManagedEventArchiveCapacity = Readonly<{
  archived_bytes: number;
  archived_events: number;
  archived_through: string;
  index_nodes: number;
  recent_descriptors: number;
  segments: number;
}>;

export type ManagedEventSealResult = Readonly<{
  archived_bytes: number;
  archived_events: number;
  end_cursor: string;
  index_node_created: boolean;
  sealed: boolean;
  segment_key?: string;
  start_cursor: string;
}>;

export type ManagedEventArchivePolicy = Readonly<{
  recentEventCount?: number;
  sealThresholdBytes?: number;
  segmentTargetBytes?: number;
}>;

/**
 * Immutable historical body for one managed AgentDO.
 *
 * SQLite owns the bounded manifest head and the mutable event tail. R2 objects
 * are content-addressed and never overwritten. Older descriptors are packed
 * into immutable ordinal index pages so hot SQLite metadata stays bounded and
 * historical reads can seek without walking the complete archive.
 */
export class ManagedEventArchive<Message extends { type: string }> {
  readonly #bucket: R2Bucket;
  readonly #prefix: string;
  readonly #recentEventCount: number;
  readonly #sealThresholdBytes: number;
  readonly #segmentTargetBytes: number;
  readonly #storage: DurableObjectStorage;

  constructor(
    storage: DurableObjectStorage,
    bucket: R2Bucket,
    agentStorageId: string,
    policy: ManagedEventArchivePolicy = {},
  ) {
    this.#storage = storage;
    this.#bucket = bucket;
    this.#prefix = `agents/${agentStorageId}/managed-events/`;
    this.#recentEventCount = boundedInteger(
      policy.recentEventCount,
      DEFAULT_RECENT_EVENT_COUNT,
      1,
      4_096,
    );
    this.#sealThresholdBytes = positiveInteger(
      policy.sealThresholdBytes,
      DEFAULT_SEAL_THRESHOLD_BYTES,
    );
    this.#segmentTargetBytes = positiveInteger(
      policy.segmentTargetBytes,
      DEFAULT_SEGMENT_TARGET_BYTES,
    );
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_event_archive_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        archived_through INTEGER NOT NULL DEFAULT 0 CHECK (archived_through >= 0),
        archived_events INTEGER NOT NULL DEFAULT 0 CHECK (archived_events >= 0),
        archived_bytes INTEGER NOT NULL DEFAULT 0 CHECK (archived_bytes >= 0),
        segment_count INTEGER NOT NULL DEFAULT 0 CHECK (segment_count >= 0),
        index_node_count INTEGER NOT NULL DEFAULT 0 CHECK (index_node_count >= 0),
        index_root_key TEXT,
        recent_json TEXT NOT NULL DEFAULT '[]'
      );
      INSERT OR IGNORE INTO managed_event_archive_state (singleton) VALUES (1);
    `);
  }

  capacity(): ManagedEventArchiveCapacity {
    const state = this.#state();
    return {
      archived_bytes: state.archived_bytes,
      archived_events: state.archived_events,
      archived_through: state.archived_through,
      index_nodes: state.index_node_count,
      recent_descriptors: this.#decodeDescriptors(state.recent_json).length,
      segments: state.segment_count,
    };
  }

  portableState(): ManagedEventArchiveState {
    return { ...this.#state() };
  }

  adoptState(state: ManagedEventArchiveState): void {
    validatePortableState(state);
    const current = this.#state();
    if (current.archived_events !== 0 || current.segment_count !== 0
      || current.index_node_count !== 0) {
      throw new Error("managed event archive adoption requires an empty destination archive");
    }
    this.#decodeDescriptors(state.recent_json);
    this.#storage.sql.exec(
      `UPDATE managed_event_archive_state
       SET archived_through = CAST(? AS INTEGER), archived_events = ?, archived_bytes = ?,
           segment_count = ?, index_node_count = ?, index_root_key = ?, recent_json = ?
       WHERE singleton = 1`,
      state.archived_through,
      state.archived_events,
      state.archived_bytes,
      state.segment_count,
      state.index_node_count,
      state.index_root_key,
      state.recent_json,
    );
  }

  archivedThrough(): string {
    return this.#state().archived_through;
  }

  latestCursor(local: DurableEventLog<Message>): string {
    return maxCursor(this.archivedThrough(), local.latestCursor());
  }

  needsSeal(local: DurableEventLog<Message>): boolean {
    return local.totalBytes() >= this.#sealThresholdBytes;
  }

  async seal(force = false): Promise<ManagedEventSealResult> {
    const state = this.#state();
    const local = this.#localAggregate();
    if (!force && local.bytes < this.#sealThresholdBytes) return emptySeal(state.archived_through);
    // Normally preserve the configured hot window. Under byte pressure, a
    // small number of individually large events must still make progress.
    const retainedRows = force
      ? Math.min(1, local.rows)
      : local.rows > this.#recentEventCount ? this.#recentEventCount : 0;
    const availableRows = Math.max(0, local.rows - retainedRows);
    if (availableRows === 0) return emptySeal(state.archived_through);

    const candidates = this.#storage.sql.exec<EventIndexRow>(
      `SELECT CAST(events.cursor AS TEXT) AS cursor,
              events.turn_id,
              events.created_at,
              LENGTH(CAST(events.message_json AS BLOB))
                + COALESCE((
                  SELECT SUM(LENGTH(CAST(chunks.message_json AS BLOB)))
                  FROM managed_event_chunks chunks
                  WHERE chunks.cursor = events.cursor
                ), 0) AS message_bytes
       FROM managed_events events
       ORDER BY events.cursor
       LIMIT ?`,
      Math.min(MAX_SEAL_ROWS, availableRows),
    ).toArray();
    const selectedCandidates: EventIndexRow[] = [];
    let selectedBytes = 0;
    for (const candidate of candidates) {
      if (selectedCandidates.length > 0
        && selectedBytes + candidate.message_bytes > this.#segmentTargetBytes) break;
      selectedCandidates.push(candidate);
      selectedBytes += candidate.message_bytes;
      if (selectedBytes >= this.#segmentTargetBytes) break;
    }
    if (selectedCandidates.length === 0) return emptySeal(state.archived_through);
    const selectedHeads = this.#storage.sql.exec<EventRow>(
      `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, message_json, created_at
       FROM managed_events
       WHERE cursor >= CAST(? AS INTEGER) AND cursor <= CAST(? AS INTEGER)
       ORDER BY managed_events.cursor`,
      selectedCandidates[0]!.cursor,
      selectedCandidates.at(-1)!.cursor,
    ).toArray();
    const selected = hydrateManagedEventRows(this.#storage, selectedHeads);
    if (selected.length !== selectedCandidates.length
      || selected.some((row, index) => (
        encoder.encode(row.message_json).byteLength !== selectedCandidates[index]!.message_bytes
      ))) {
      throw new Error("managed event archive source changed during selection");
    }
    if (BigInt(selected[0]!.cursor) <= BigInt(state.archived_through)) {
      throw new Error("managed event archive source is not newer than its ownership fence");
    }

    const events = selected.map((row) => ({
      cursor: row.cursor,
      created_at: row.created_at,
      message: JSON.parse(row.message_json) as Message,
      turn_id: row.turn_id,
    }));
    const segmentBody = JSON.stringify({
      version: VERSION,
      kind: "managed_event_segment",
      events,
    } satisfies SegmentEnvelope<Message>);
    const segmentBytes = encoder.encode(segmentBody);
    const segmentHash = await sha256Hex(segmentBytes);
    const descriptor: SegmentDescriptor = {
      bytes: segmentBytes.byteLength,
      count: selected.length,
      // Source-derived so a crash after an immutable R2 put can retry the same
      // ordinal index page byte-for-byte before SQLite advances its fence.
      created_at: selected.at(-1)!.created_at,
      end_cursor: selected.at(-1)!.cursor,
      key: `${this.#prefix}segments/${padCursor(selected[0]!.cursor)}-${padCursor(selected.at(-1)!.cursor)}-${segmentHash}.json`,
      start_cursor: selected[0]!.cursor,
    };
    await this.#putImmutable(descriptor.key, segmentBytes, "managed_event_segment", segmentHash);

    const currentRecent = this.#decodeDescriptors(state.recent_json);
    if (currentRecent.at(-1)?.end_cursor !== undefined
      && currentRecent.at(-1)!.end_cursor !== state.archived_through) {
      throw new Error("managed event archive descriptor head does not match its ownership fence");
    }
    let nextRecent = [...currentRecent, descriptor];
    let nextRoot = state.index_root_key;
    let indexCreated = false;
    let indexKey: string | undefined;
    if (nextRecent.length > MAX_RECENT_DESCRIPTORS) {
      const packed = nextRecent.slice(0, MAX_RECENT_DESCRIPTORS);
      nextRecent = nextRecent.slice(MAX_RECENT_DESCRIPTORS);
      const indexBody = JSON.stringify({
        version: VERSION,
        kind: "managed_event_index",
        ordinal: state.index_node_count,
        descriptors: packed,
      } satisfies IndexEnvelope);
      const encodedIndex = encoder.encode(indexBody);
      const indexHash = await sha256Hex(encodedIndex);
      indexKey = this.#indexKey(state.index_node_count);
      await this.#putImmutable(indexKey, encodedIndex, "managed_event_index", indexHash);
      nextRoot = indexKey;
      indexCreated = true;
    }

    this.#storage.transactionSync(() => {
      const retained = this.#state();
      if (retained.archived_through !== state.archived_through
        || retained.index_root_key !== state.index_root_key
        || retained.recent_json !== state.recent_json) {
        throw new Error("managed event archive seal lost its SQLite ownership fence");
      }
      const retainedHeads = this.#storage.sql.exec<EventRow>(
        `SELECT CAST(cursor AS TEXT) AS cursor, turn_id, message_json, created_at
         FROM managed_events
         WHERE cursor >= CAST(? AS INTEGER) AND cursor <= CAST(? AS INTEGER)
         ORDER BY managed_events.cursor`,
        descriptor.start_cursor,
        descriptor.end_cursor,
      ).toArray();
      const retainedRows = hydrateManagedEventRows(this.#storage, retainedHeads);
      const sourceUnchanged = retainedRows.length === selected.length
        && retainedRows.every((row, index) => {
          const source = selected[index]!;
          return row.cursor === source.cursor
            && row.turn_id === source.turn_id
            && row.message_json === source.message_json
            && row.created_at === source.created_at;
        });
      if (!sourceUnchanged) {
        throw new Error(
          `managed event archive source prefix changed before commit (rows ${retainedRows.length}/${descriptor.count})`,
        );
      }
      this.#storage.sql.exec(
        `UPDATE managed_event_archive_state
         SET archived_through = CAST(? AS INTEGER),
             archived_events = archived_events + ?,
             archived_bytes = archived_bytes + ?,
             segment_count = segment_count + 1,
             index_node_count = index_node_count + ?,
             index_root_key = ?,
             recent_json = ?
         WHERE singleton = 1`,
        descriptor.end_cursor,
        descriptor.count,
        descriptor.bytes,
        indexCreated ? 1 : 0,
        nextRoot,
        JSON.stringify(nextRecent),
      );
      this.#storage.sql.exec(
        "DELETE FROM managed_event_chunks WHERE cursor <= CAST(? AS INTEGER)",
        descriptor.end_cursor,
      );
      this.#storage.sql.exec(
        "DELETE FROM managed_events WHERE cursor <= CAST(? AS INTEGER)",
        descriptor.end_cursor,
      );
      this.#storage.sql.exec(
        `UPDATE managed_event_meta
         SET total_bytes = CASE WHEN total_bytes >= ? THEN total_bytes - ? ELSE 0 END
         WHERE singleton = 1`,
        selectedBytes,
        selectedBytes,
      );
    });

    return {
      archived_bytes: descriptor.bytes,
      archived_events: descriptor.count,
      end_cursor: descriptor.end_cursor,
      index_node_created: indexCreated,
      sealed: true,
      segment_key: descriptor.key,
      start_cursor: descriptor.start_cursor,
    };
  }

  async pageAfter(
    after: string,
    limit: number,
    cache: SegmentReadCache<Message> = {},
  ): Promise<DurableEvent<Message>[]> {
    const state = this.#state();
    if (BigInt(after) >= BigInt(state.archived_through)) return [];
    const descriptors = await this.#descriptorsAfter(after, limit);
    const events: DurableEvent<Message>[] = [];
    for (const descriptor of descriptors) {
      const segment = await this.#readSegment(descriptor, cache);
      for (const event of segment) {
        if (BigInt(event.cursor) <= BigInt(after)) continue;
        events.push(event);
        if (events.length >= limit) return events;
      }
    }
    return events;
  }

  async page(
    local: DurableEventLog<Message>,
    after: string,
    limit: number,
  ): Promise<DurableEvent<Message>[]> {
    return this.#page(local, after, limit, {});
  }

  pageReader(
    local: DurableEventLog<Message>,
  ): (after: string, limit: number) => Promise<DurableEvent<Message>[]> {
    const cache: SegmentReadCache<Message> = {};
    return (after, limit) => this.#page(local, after, limit, cache);
  }

  async #page(
    local: DurableEventLog<Message>,
    after: string,
    limit: number,
    cache: SegmentReadCache<Message>,
  ): Promise<DurableEvent<Message>[]> {
    while (true) {
      const fence = this.#readFence();
      const archived = await this.pageAfter(after, limit, cache);
      const localAfter = archived.at(-1)?.cursor ?? after;
      const tail = archived.length >= limit ? [] : local.page(localAfter, limit - archived.length);
      if (sameFence(fence, this.#readFence())) return [...archived, ...tail];
    }
  }

  async history(
    local: DurableEventLog<Message>,
    before: string | undefined,
    limit: number,
  ): Promise<DurableEventHistory<Message>> {
    const cache: SegmentReadCache<Message> = {};
    while (true) {
      const fence = this.#readFence();
      const localPage = local.history(before, limit);
      let page: DurableEventHistory<Message>;
      if (localPage.data.length >= limit) {
        page = {
          ...localPage,
          has_more: localPage.has_more || fence.archived_events > 0,
          latest_cursor: maxCursor(localPage.latest_cursor, fence.archived_through),
        };
      } else if (fence.archived_events === 0) {
        page = localPage;
      } else {
        const archiveBefore = localPage.data[0]?.cursor ?? before;
        const remaining = limit - localPage.data.length;
        const archived = await this.#historyBefore(archiveBefore, remaining, cache);
        page = {
          data: [...archived.data, ...localPage.data],
          has_more: archived.has_more,
          latest_cursor: maxCursor(localPage.latest_cursor, fence.archived_through),
        };
      }
      if (sameFence(fence, this.#readFence())) return page;
    }
  }

  async deleteAll(): Promise<number> {
    let deleted = 0;
    while (true) {
      const listed = await this.#bucket.list({ prefix: this.#prefix, limit: 1_000 });
      const keys = listed.objects.map(({ key }) => key);
      if (keys.length === 0) return deleted;
      await this.#bucket.delete(keys);
      deleted += keys.length;
    }
  }

  clearLocalState(): void {
    this.#storage.sql.exec(`
      UPDATE managed_event_archive_state
      SET archived_through = 0,
          archived_events = 0,
          archived_bytes = 0,
          segment_count = 0,
          index_node_count = 0,
          index_root_key = NULL,
          recent_json = '[]'
      WHERE singleton = 1
    `);
  }

  async #descriptorsAfter(after: string, limit: number): Promise<SegmentDescriptor[]> {
    const state = this.#state();
    const nodes = new Map<number, IndexEnvelope>();
    const read = async (ordinal: number): Promise<IndexEnvelope> => {
      const cached = nodes.get(ordinal);
      if (cached) return cached;
      const node = await this.#readIndex(ordinal);
      nodes.set(ordinal, node);
      return node;
    };
    let low = 0;
    let high = state.index_node_count;
    while (low < high) {
      const middle = Math.floor((low + high) / 2);
      const node = await read(middle);
      if (BigInt(node.descriptors.at(-1)!.end_cursor) > BigInt(after)) high = middle;
      else low = middle + 1;
    }
    const retained: SegmentDescriptor[] = [];
    for (let ordinal = low; ordinal < state.index_node_count && retained.length < limit; ordinal++) {
      const node = await read(ordinal);
      retained.push(...node.descriptors.filter(
        ({ end_cursor }) => BigInt(end_cursor) > BigInt(after),
      ).slice(0, limit - retained.length));
    }
    if (retained.length < limit) {
      retained.push(...this.#decodeDescriptors(state.recent_json).filter(
        ({ end_cursor }) => BigInt(end_cursor) > BigInt(after),
      ).slice(0, limit - retained.length));
    }
    return retained;
  }

  async #historyBefore(
    before: string | undefined,
    limit: number,
    cache: SegmentReadCache<Message>,
  ): Promise<{ data: DurableEvent<Message>[]; has_more: boolean }> {
    if (limit <= 0) return { data: [], has_more: this.#state().archived_events > 0 };
    const boundary = BigInt(before ?? (BigInt(this.archivedThrough()) + 1n).toString());
    const collected: DurableEvent<Message>[] = [];
    const consume = async (descriptors: SegmentDescriptor[]): Promise<boolean> => {
      for (const descriptor of [...descriptors].reverse()) {
        if (BigInt(descriptor.start_cursor) >= boundary) continue;
        const segment = await this.#readSegment(descriptor, cache);
        for (const event of [...segment].reverse()) {
          if (BigInt(event.cursor) >= boundary) continue;
          collected.push(event);
          if (collected.length > limit) return true;
        }
      }
      return false;
    };
    const state = this.#state();
    if (!await consume(this.#decodeDescriptors(state.recent_json))) {
      const nodes = new Map<number, IndexEnvelope>();
      const read = async (ordinal: number): Promise<IndexEnvelope> => {
        const cached = nodes.get(ordinal);
        if (cached) return cached;
        const node = await this.#readIndex(ordinal);
        nodes.set(ordinal, node);
        return node;
      };
      let low = 0;
      let high = state.index_node_count;
      while (low < high) {
        const middle = Math.floor((low + high) / 2);
        const node = await read(middle);
        if (BigInt(node.descriptors[0]!.start_cursor) < boundary) low = middle + 1;
        else high = middle;
      }
      for (let ordinal = low - 1; ordinal >= 0; ordinal--) {
        const node = await read(ordinal);
        if (await consume(node.descriptors)) break;
      }
    }
    const hasMore = collected.length > limit;
    if (hasMore) collected.pop();
    return { data: collected.reverse(), has_more: hasMore };
  }

  async #readSegment(
    descriptor: SegmentDescriptor,
    cache: SegmentReadCache<Message>,
  ): Promise<DurableEvent<Message>[]> {
    if (cache.segment?.key === descriptor.key) return cache.segment.events;
    const object = await this.#bucket.get(this.#portableObjectKey(descriptor.key));
    if (!object || !object.body) throw new Error("managed event archive segment is unavailable");
    if (object.size !== descriptor.bytes) {
      await object.body.cancel();
      throw new Error("managed event archive segment size does not match its descriptor");
    }
    const encoded = new Uint8Array(await object.arrayBuffer());
    const expectedHash = object.customMetadata?.sha256;
    if (!expectedHash || object.customMetadata?.kind !== "managed_event_segment"
      || object.customMetadata?.version !== String(VERSION)
      || await sha256Hex(encoded) !== expectedHash) {
      throw new Error("managed event archive segment checksum mismatch");
    }
    const value = JSON.parse(new TextDecoder().decode(encoded)) as SegmentEnvelope<Message>;
    if (value.version !== VERSION || value.kind !== "managed_event_segment"
      || !Array.isArray(value.events) || value.events.length !== descriptor.count
      || value.events[0]?.cursor !== descriptor.start_cursor
      || value.events.at(-1)?.cursor !== descriptor.end_cursor) {
      throw new Error("managed event archive segment is invalid");
    }
    for (const [index, event] of value.events.entries()) {
      if (!event || typeof event !== "object"
        || typeof event.cursor !== "string" || !/^[1-9][0-9]*$/.test(event.cursor)
        || !Number.isSafeInteger(event.created_at) || event.created_at < 0
        || (event.turn_id !== null && typeof event.turn_id !== "string")
        || !event.message || typeof event.message !== "object"
        || typeof event.message.type !== "string"
        || (index > 0 && BigInt(value.events[index - 1]!.cursor) >= BigInt(event.cursor))) {
        throw new Error("managed event archive segment contains an invalid event sequence");
      }
    }
    cache.segment = { events: value.events, key: descriptor.key };
    return value.events;
  }

  async #readIndex(ordinal: number): Promise<IndexEnvelope> {
    const key = this.#indexKey(ordinal);
    const object = await this.#bucket.get(key);
    if (!object || !object.body) throw new Error("managed event archive index is unavailable");
    const encoded = new Uint8Array(await object.arrayBuffer());
    const expectedHash = object.customMetadata?.sha256;
    if (!expectedHash || object.customMetadata?.kind !== "managed_event_index"
      || object.customMetadata?.version !== String(VERSION)
      || await sha256Hex(encoded) !== expectedHash) {
      throw new Error("managed event archive index checksum mismatch");
    }
    const value = JSON.parse(new TextDecoder().decode(encoded)) as IndexEnvelope;
    if (value.version !== VERSION || value.kind !== "managed_event_index"
      || value.ordinal !== ordinal
      || !Array.isArray(value.descriptors)
      || value.descriptors.length !== MAX_RECENT_DESCRIPTORS) {
      throw new Error("managed event archive index is invalid");
    }
    this.#decodeDescriptors(JSON.stringify(value.descriptors));
    return value;
  }

  #portableObjectKey(key: string): string {
    const marker = "/managed-events/";
    const markerOffset = key.indexOf(marker);
    const suffix = markerOffset === -1 ? key : key.slice(markerOffset + marker.length);
    return key.startsWith(this.#prefix) ? key : `${this.#prefix}${suffix}`;
  }

  async #putImmutable(
    key: string,
    body: Uint8Array,
    kind: SegmentEnvelope<Message>["kind"] | IndexEnvelope["kind"],
    sha256: string,
  ): Promise<void> {
    const stored = await this.#bucket.put(key, body, {
      onlyIf: { etagDoesNotMatch: "*" },
      httpMetadata: { contentType: "application/json" },
      customMetadata: { kind, sha256, version: String(VERSION) },
      sha256,
    });
    if (stored) return;
    const existing = await this.#bucket.head(key);
    if (!existing || existing.size !== body.byteLength
      || existing.customMetadata?.sha256 !== sha256
      || existing.customMetadata?.kind !== kind) {
      throw new Error("managed event archive content-addressed object conflicts with existing data");
    }
  }

  #localAggregate(): { bytes: number; rows: number } {
    return this.#storage.sql.exec<{ bytes: number; rows: number }>(
      `SELECT COUNT(*) AS rows,
              COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
                + (SELECT COALESCE(SUM(LENGTH(CAST(message_json AS BLOB))), 0)
                   FROM managed_event_chunks) AS bytes
       FROM managed_events`,
    ).toArray()[0] ?? { bytes: 0, rows: 0 };
  }

  #state(): ArchiveStateRow {
    const row = this.#storage.sql.exec<ArchiveStateRow>(
      `SELECT CAST(archived_through AS TEXT) AS archived_through,
              archived_events, archived_bytes, segment_count, index_node_count,
              index_root_key, recent_json
       FROM managed_event_archive_state WHERE singleton = 1`,
    ).toArray()[0];
    if (!row) throw new Error("managed event archive state is unavailable");
    this.#decodeDescriptors(row.recent_json);
    if (row.index_root_key !== null
      && !/^agents\/[0-9a-f]{64}\/managed-events\/indexes\/[0-9]{16}\.json$/.test(
        row.index_root_key,
      )) {
      throw new Error("managed event archive root escapes its agent namespace");
    }
    if ((row.index_node_count === 0) !== (row.index_root_key === null)) {
      throw new Error("managed event archive index manifest is inconsistent");
    }
    if (row.index_node_count > 0
      && this.#portableObjectKey(row.index_root_key!) !== this.#indexKey(row.index_node_count - 1)) {
      throw new Error("managed event archive index root does not match its manifest");
    }
    return row;
  }

  #indexKey(ordinal: number): string {
    if (!Number.isSafeInteger(ordinal) || ordinal < 0) {
      throw new Error("managed event archive index ordinal is invalid");
    }
    return `${this.#prefix}indexes/${String(ordinal).padStart(16, "0")}.json`;
  }

  #readFence(): ArchiveReadFence {
    const state = this.#state();
    return {
      archived_events: state.archived_events,
      archived_through: state.archived_through,
    };
  }

  #decodeDescriptors(encoded: string): SegmentDescriptor[] {
    const descriptors = decodeDescriptors(encoded);
    if (descriptors.some(({ key }) => (
      !/^agents\/[0-9a-f]{64}\/managed-events\/segments\/[0-9]{19}-[0-9]{19}-[0-9a-f]{64}\.json$/.test(
        key,
      )
    ))) {
      throw new Error("managed event archive descriptor escapes its agent namespace");
    }
    return descriptors;
  }
}

function decodeDescriptors(encoded: string): SegmentDescriptor[] {
  let value: unknown;
  try { value = JSON.parse(encoded); }
  catch { throw new Error("managed event archive descriptor window is invalid JSON"); }
  if (!Array.isArray(value) || value.length > MAX_RECENT_DESCRIPTORS) {
    throw new Error("managed event archive descriptor window is invalid");
  }
  const descriptors = value as Partial<SegmentDescriptor>[];
  for (const [index, descriptor] of descriptors.entries()) {
    if (typeof descriptor.key !== "string"
      || typeof descriptor.start_cursor !== "string"
      || typeof descriptor.end_cursor !== "string"
      || !/^[0-9]+$/.test(descriptor.start_cursor)
      || !/^[0-9]+$/.test(descriptor.end_cursor)
      || BigInt(descriptor.start_cursor) > BigInt(descriptor.end_cursor)
      || !Number.isSafeInteger(descriptor.bytes) || descriptor.bytes! < 1
      || !Number.isSafeInteger(descriptor.count) || descriptor.count! < 1
      || !Number.isSafeInteger(descriptor.created_at) || descriptor.created_at! < 0) {
      throw new Error("managed event archive descriptor is invalid");
    }
    if (index > 0
      && BigInt(descriptors[index - 1]!.end_cursor!) >= BigInt(descriptor.start_cursor)) {
      throw new Error("managed event archive descriptors overlap or are unordered");
    }
  }
  return descriptors as SegmentDescriptor[];
}

function validatePortableState(value: ManagedEventArchiveState): void {
  if (!value || typeof value !== "object"
    || typeof value.archived_through !== "string"
    || !/^[0-9]+$/.test(value.archived_through)
    || !Number.isSafeInteger(value.archived_events) || value.archived_events < 0
    || !Number.isSafeInteger(value.archived_bytes) || value.archived_bytes < 0
    || !Number.isSafeInteger(value.segment_count) || value.segment_count < 0
    || !Number.isSafeInteger(value.index_node_count) || value.index_node_count < 0
    || (value.index_root_key !== null && typeof value.index_root_key !== "string")
    || typeof value.recent_json !== "string"
    || (value.index_node_count === 0) !== (value.index_root_key === null)
    || value.archived_events < value.segment_count) {
    throw new Error("managed event archive portable state is invalid");
  }
}

function boundedInteger(
  value: number | undefined,
  fallback: number,
  minimum: number,
  maximum: number,
): number {
  return Number.isSafeInteger(value) ? Math.min(maximum, Math.max(minimum, value!)) : fallback;
}

function positiveInteger(value: number | undefined, fallback: number): number {
  return Number.isSafeInteger(value) && value! > 0 ? value! : fallback;
}

function emptySeal(cursor: string): ManagedEventSealResult {
  return {
    archived_bytes: 0,
    archived_events: 0,
    end_cursor: cursor,
    index_node_created: false,
    sealed: false,
    start_cursor: cursor,
  };
}

function compareCursor(left: string, right: string): number {
  const a = BigInt(left);
  const b = BigInt(right);
  return a < b ? -1 : a > b ? 1 : 0;
}

function maxCursor(left: string, right: string): string {
  return compareCursor(left, right) >= 0 ? left : right;
}

function sameFence(left: ArchiveReadFence, right: ArchiveReadFence): boolean {
  return left.archived_events === right.archived_events
    && left.archived_through === right.archived_through;
}

function padCursor(cursor: string): string {
  return cursor.padStart(19, "0");
}
