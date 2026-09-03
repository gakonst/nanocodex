import { sha256Hex } from "./archive-hash";

const VERSION = 1;
const DEFAULT_RECENT_TERMINAL_TURNS = 512;
const MAX_SEAL_RECEIPTS = 32;
const TRANSFER_BATCH_OBJECTS = 64;
const TRANSFER_BATCH_BYTES = 8 * 1024 * 1024;
const TRANSFER_COPY_CONCURRENCY = 4;
const EMPTY_TRANSFER_DIGEST = "0".repeat(64);
const encoder = new TextEncoder();

export type ManagedTurnReceipt = Readonly<{
  accepted_at: number;
  accepted_cursor: string | null;
  attempt_count: number;
  created_at: number;
  error: string | null;
  id: string;
  input_json: string;
  may_have_inner_operation: number;
  request_hash: string;
  request_key: string | null;
  retry_at: number | null;
  state: "completed" | "cancelled" | "failed";
  terminal_cursor: string | null;
  terminal_json: string;
  updated_at: number;
}>;

type ReceiptEnvelope = Readonly<{
  kind: "managed_turn_receipt";
  receipt: ManagedTurnReceipt;
  version: 1;
}>;

type ArchiveState = {
  archived_bytes: number;
  archived_receipts: number;
  object_count: number;
};

export type ManagedTurnArchiveCapacity = Readonly<{
  archived_bytes: number;
  archived_receipts: number;
  objects: number;
}>;

export type ManagedTurnSealResult = Readonly<{
  archived_bytes: number;
  archived_receipts: number;
  objects: number;
  sealed: boolean;
}>;

export type ManagedTurnArchiveIdentity = Readonly<{
  archived_bytes: number;
  archived_receipts: number;
  digest: string;
  objects: number;
  version: 1;
}>;

export type ManagedTurnArchiveTransferResult = Readonly<{
  complete: boolean;
  identity?: ManagedTurnArchiveIdentity;
  objects: number;
}>;

type ArchivedObject = Readonly<{
  sha256: string;
  size: number;
  suffix: string;
}>;

type TransferProgress = {
  archived_bytes: number;
  archived_receipts: number;
  complete: number;
  digest: string;
  last_key: string | null;
  object_count: number;
};

/** Immutable terminal receipts kept outside the bounded coordination head. */
export class ManagedTurnArchive {
  readonly #bucket: R2Bucket;
  readonly #prefix: string;
  readonly #recentTerminalTurns: number;
  readonly #storage: DurableObjectStorage;

  constructor(
    storage: DurableObjectStorage,
    bucket: R2Bucket,
    agentStorageId: string,
    recentTerminalTurns = DEFAULT_RECENT_TERMINAL_TURNS,
  ) {
    this.#storage = storage;
    this.#bucket = bucket;
    this.#prefix = `agents/${agentStorageId}/managed-turns/`;
    this.#recentTerminalTurns = Number.isSafeInteger(recentTerminalTurns)
      ? Math.min(4_096, Math.max(1, recentTerminalTurns))
      : DEFAULT_RECENT_TERMINAL_TURNS;
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_turn_archive_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        archived_receipts INTEGER NOT NULL DEFAULT 0 CHECK (archived_receipts >= 0),
        archived_bytes INTEGER NOT NULL DEFAULT 0 CHECK (archived_bytes >= 0),
        object_count INTEGER NOT NULL DEFAULT 0 CHECK (object_count >= 0)
      );
      INSERT OR IGNORE INTO managed_turn_archive_state (singleton) VALUES (1);
      CREATE TABLE IF NOT EXISTS managed_turn_archive_adoption (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        manifest_digest TEXT NOT NULL,
        source_storage_id TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS managed_turn_archive_manifest_progress (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        last_key TEXT,
        digest TEXT NOT NULL,
        archived_bytes INTEGER NOT NULL DEFAULT 0,
        archived_receipts INTEGER NOT NULL DEFAULT 0,
        object_count INTEGER NOT NULL DEFAULT 0,
        complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1))
      );
      INSERT OR IGNORE INTO managed_turn_archive_manifest_progress (
        singleton, digest
      ) VALUES (1, '${EMPTY_TRANSFER_DIGEST}');
      CREATE TABLE IF NOT EXISTS managed_turn_archive_adoption_progress (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        source_storage_id TEXT NOT NULL,
        manifest_digest TEXT NOT NULL,
        last_key TEXT,
        scan_digest TEXT NOT NULL,
        archived_bytes INTEGER NOT NULL DEFAULT 0,
        archived_receipts INTEGER NOT NULL DEFAULT 0,
        object_count INTEGER NOT NULL DEFAULT 0,
        complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1))
      );
    `);
  }

  capacity(): ManagedTurnArchiveCapacity {
    const state = this.#state();
    return {
      archived_bytes: state.archived_bytes,
      archived_receipts: state.archived_receipts,
      objects: state.object_count,
    };
  }

  needsSeal(): boolean {
    return this.#terminalCount() > this.#recentTerminalTurns;
  }

  async findById(id: string): Promise<ManagedTurnReceipt | undefined> {
    if (this.#state().archived_receipts === 0) return undefined;
    const receipt = await this.#read(`${this.#prefix}by-id/${await sha256Hex(encoder.encode(id))}.json`);
    if (receipt && receipt.id !== id) {
      throw new Error("managed turn archive ID lookup returned a conflicting receipt");
    }
    return receipt;
  }

  async findByRequestKey(requestKey: string): Promise<ManagedTurnReceipt | undefined> {
    if (this.#state().archived_receipts === 0) return undefined;
    const receipt = await this.#read(
      `${this.#prefix}by-request/${await sha256Hex(encoder.encode(requestKey))}.json`,
    );
    if (receipt && receipt.request_key !== requestKey) {
      throw new Error("managed turn archive request lookup returned a conflicting receipt");
    }
    return receipt;
  }

  async seal(
    force = false,
    retainTerminalTurns?: number,
  ): Promise<ManagedTurnSealResult> {
    const terminalCount = this.#terminalCount();
    const retained = retainTerminalTurns === undefined
      ? (force ? Math.min(1, terminalCount) : this.#recentTerminalTurns)
      : Math.min(terminalCount, Math.max(0, retainTerminalTurns));
    const available = Math.max(
      0,
      terminalCount - retained,
    );
    if (available === 0) return emptySeal();
    const receipts = this.#storage.sql.exec<ManagedTurnReceipt>(
      `${RECEIPT_SELECT}
       WHERE state IN ('completed', 'cancelled', 'failed')
         AND terminal_json IS NOT NULL
       ORDER BY updated_at, created_at, id
       LIMIT ?`,
      Math.min(MAX_SEAL_RECEIPTS, available),
    ).toArray();
    if (receipts.length === 0) return emptySeal();

    const encoded = await Promise.all(receipts.map(async (receipt) => {
      validateReceipt(receipt);
      const body = encoder.encode(JSON.stringify({
        version: VERSION,
        kind: "managed_turn_receipt",
        receipt,
      } satisfies ReceiptEnvelope));
      const bodyHash = await sha256Hex(body);
      const keys = [
        `${this.#prefix}by-id/${await sha256Hex(encoder.encode(receipt.id))}.json`,
        ...(receipt.request_key === null ? [] : [
          `${this.#prefix}by-request/${await sha256Hex(encoder.encode(receipt.request_key))}.json`,
        ]),
      ];
      await Promise.all(keys.map((key) => this.#putImmutable(key, body, bodyHash)));
      return { bodyBytes: body.byteLength, keys, receipt };
    }));

    this.#storage.transactionSync(() => {
      for (const item of encoded) {
        const retained = this.#storage.sql.exec<ManagedTurnReceipt>(
          `${RECEIPT_SELECT} WHERE id = ?`,
          item.receipt.id,
        ).toArray()[0];
        if (!retained || JSON.stringify(retained) !== JSON.stringify(item.receipt)) {
          throw new Error("managed turn archive receipt changed before commit");
        }
      }
      this.#storage.sql.exec(
        `DELETE FROM managed_turn_dispatch_chunks
         WHERE turn_id IN (${encoded.map(() => "?").join(",")})`,
        ...encoded.map(({ receipt }) => receipt.id),
      );
      this.#storage.sql.exec(
        `DELETE FROM managed_turns WHERE id IN (${encoded.map(() => "?").join(",")})`,
        ...encoded.map(({ receipt }) => receipt.id),
      );
      this.#storage.sql.exec(
        `UPDATE managed_turn_archive_state
         SET archived_receipts = archived_receipts + ?,
             archived_bytes = archived_bytes + ?,
             object_count = object_count + ?
         WHERE singleton = 1`,
        encoded.length,
        encoded.reduce((sum, item) => sum + item.bodyBytes * item.keys.length, 0),
        encoded.reduce((sum, item) => sum + item.keys.length, 0),
      );
    });
    return {
      archived_bytes: encoded.reduce((sum, item) => sum + item.bodyBytes * item.keys.length, 0),
      archived_receipts: encoded.length,
      objects: encoded.reduce((sum, item) => sum + item.keys.length, 0),
      sealed: true,
    };
  }

  async identityBatch(): Promise<ManagedTurnArchiveTransferResult> {
    const progress = this.#storage.sql.exec<TransferProgress>(
      `SELECT last_key, digest, archived_bytes, archived_receipts,
              object_count, complete
       FROM managed_turn_archive_manifest_progress WHERE singleton = 1`,
    ).one();
    if (progress.complete === 1) {
      return { complete: true, identity: identityFromProgress(progress), objects: 0 };
    }
    const page = await this.#archivePage(this.#prefix, progress.last_key);
    const next = await advanceProgress(progress, page.items);
    const complete = !page.truncated;
    if (complete) this.#assertCommittedIdentity(next);
    this.#storage.sql.exec(
      `UPDATE managed_turn_archive_manifest_progress
       SET last_key = ?, digest = ?, archived_bytes = ?, archived_receipts = ?,
           object_count = ?, complete = ? WHERE singleton = 1`,
      page.lastKey ?? progress.last_key,
      next.digest,
      next.archived_bytes,
      next.archived_receipts,
      next.object_count,
      complete ? 1 : 0,
    );
    return {
      complete,
      ...(complete ? { identity: identityFromProgress(next) } : {}),
      objects: page.items.length,
    };
  }

  async adoptBatch(
    sourceStorageId: string,
    expected: ManagedTurnArchiveIdentity,
    assertOwnership: () => void = () => {},
  ): Promise<ManagedTurnArchiveTransferResult> {
    assertOwnership();
    if (!/^[0-9a-f]{64}$/.test(sourceStorageId)) {
      throw new Error("managed turn archive adoption source is invalid");
    }
    validateIdentity(expected);
    let progress = this.#storage.sql.exec<TransferProgress & {
      manifest_digest: string;
      source_storage_id: string;
    }>(
      `SELECT source_storage_id, manifest_digest, last_key,
              scan_digest AS digest, archived_bytes, archived_receipts,
              object_count, complete
       FROM managed_turn_archive_adoption_progress WHERE singleton = 1`,
    ).toArray()[0];
    if (progress) {
      if (progress.manifest_digest !== expected.digest
        || progress.source_storage_id !== sourceStorageId) {
        throw new Error("managed turn archive adoption conflicts with retained identity");
      }
      if (progress.complete === 1) return { complete: true, identity: expected, objects: 0 };
    } else {
      this.#storage.sql.exec(
        `INSERT INTO managed_turn_archive_adoption_progress (
           singleton, source_storage_id, manifest_digest, scan_digest
         ) VALUES (1, ?, ?, ?)`,
        sourceStorageId,
        expected.digest,
        EMPTY_TRANSFER_DIGEST,
      );
      progress = {
        archived_bytes: 0,
        archived_receipts: 0,
        complete: 0,
        digest: EMPTY_TRANSFER_DIGEST,
        last_key: null,
        manifest_digest: expected.digest,
        object_count: 0,
        source_storage_id: sourceStorageId,
      };
    }
    if (sourceStorageId === this.#prefix.split("/")[1]) {
      throw new Error("managed turn archive cannot adopt itself");
    }
    const sourcePrefix = `agents/${sourceStorageId}/managed-turns/`;
    const page = await this.#archivePage(sourcePrefix, progress.last_key, assertOwnership);
    assertOwnership();
    await mapWithConcurrency(page.items, TRANSFER_COPY_CONCURRENCY, async (item) => {
      assertOwnership();
      const source = await this.#bucket.get(`${sourcePrefix}${item.suffix}`);
      assertOwnership();
      if (!source || !source.body || source.size !== item.size) {
        await source?.body?.cancel();
        throw new Error("managed turn archive adoption source object is unavailable");
      }
      const body = new Uint8Array(await source.arrayBuffer());
      assertOwnership();
      if (await sha256Hex(body) !== item.sha256
        || source.customMetadata?.kind !== "managed_turn_receipt"
        || source.customMetadata?.version !== String(VERSION)
        || source.customMetadata?.sha256 !== item.sha256) {
        throw new Error("managed turn archive adoption source checksum mismatch");
      }
      assertOwnership();
      const envelope = JSON.parse(new TextDecoder().decode(body)) as ReceiptEnvelope;
      if (envelope.version !== VERSION || envelope.kind !== "managed_turn_receipt") {
        throw new Error("managed turn archive adoption source envelope is invalid");
      }
      validateReceipt(envelope.receipt);
      const expectedSuffix = item.suffix.startsWith("by-id/")
        ? `by-id/${await sha256Hex(encoder.encode(envelope.receipt.id))}.json`
        : envelope.receipt.request_key === null
        ? undefined
        : `by-request/${await sha256Hex(encoder.encode(envelope.receipt.request_key))}.json`;
      assertOwnership();
      if (expectedSuffix !== item.suffix) {
        throw new Error("managed turn archive adoption source key conflicts with receipt");
      }
      await this.#putImmutable(
        `${this.#prefix}${item.suffix}`,
        body,
        item.sha256,
        assertOwnership,
      );
    });
    assertOwnership();
    const next = await advanceProgress(progress, page.items);
    assertOwnership();
    const complete = !page.truncated;
    if (complete && JSON.stringify(identityFromProgress(next)) !== JSON.stringify(expected)) {
      throw new Error("managed turn archive source identity changed during adoption");
    }
    this.#storage.transactionSync(() => {
      assertOwnership();
      this.#storage.sql.exec(
        `UPDATE managed_turn_archive_adoption_progress
         SET last_key = ?, scan_digest = ?, archived_bytes = ?, archived_receipts = ?,
             object_count = ?, complete = ? WHERE singleton = 1`,
        page.lastKey ?? progress.last_key,
        next.digest,
        next.archived_bytes,
        next.archived_receipts,
        next.object_count,
        complete ? 1 : 0,
      );
      if (!complete) return;
      const state = this.#state();
      if (state.archived_bytes !== 0 || state.archived_receipts !== 0 || state.object_count !== 0) {
        throw new Error("managed turn archive adoption requires an empty destination archive");
      }
      this.#storage.sql.exec(
        `UPDATE managed_turn_archive_state
         SET archived_receipts = ?, archived_bytes = ?, object_count = ? WHERE singleton = 1`,
        expected.archived_receipts, expected.archived_bytes, expected.objects,
      );
      this.#storage.sql.exec(
        `INSERT INTO managed_turn_archive_adoption (
           singleton, manifest_digest, source_storage_id
         ) VALUES (1, ?, ?)`,
        expected.digest,
        sourceStorageId,
      );
    });
    return { complete, ...(complete ? { identity: expected } : {}), objects: page.items.length };
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
      UPDATE managed_turn_archive_state
      SET archived_receipts = 0, archived_bytes = 0, object_count = 0
      WHERE singleton = 1;
      DELETE FROM managed_turn_archive_adoption;
      DELETE FROM managed_turn_archive_adoption_progress;
      UPDATE managed_turn_archive_manifest_progress
      SET last_key = NULL, digest = '${EMPTY_TRANSFER_DIGEST}', archived_bytes = 0,
          archived_receipts = 0, object_count = 0, complete = 0
      WHERE singleton = 1;
    `);
  }

  async #archivePage(
    prefix: string,
    lastKey: string | null,
    assertOwnership: () => void = () => {},
  ): Promise<{
    items: ArchivedObject[];
    lastKey?: string;
    truncated: boolean;
  }> {
      assertOwnership();
      const page = await this.#bucket.list({
        prefix,
        limit: TRANSFER_BATCH_OBJECTS,
        ...(lastKey === null ? {} : { startAfter: lastKey }),
        include: ["customMetadata"],
      });
      assertOwnership();
      const items: ArchivedObject[] = [];
      let selectedBytes = 0;
      for (const object of page.objects) {
        const suffix = object.key.slice(prefix.length);
        if (!/^(?:by-id|by-request)\/[0-9a-f]{64}\.json$/.test(suffix)
          || object.size <= 0
          || object.customMetadata?.kind !== "managed_turn_receipt"
          || object.customMetadata?.version !== String(VERSION)
          || !/^[0-9a-f]{64}$/.test(object.customMetadata?.sha256 ?? "")) {
          throw new Error("managed turn archive contains an invalid object identity");
        }
        // Always make progress on the first valid object, then stop before the
        // aggregate body budget is exceeded. The durable last key makes the
        // unselected suffix of this list page the next resumable batch.
        if (items.length > 0 && selectedBytes + object.size > TRANSFER_BATCH_BYTES) break;
        items.push({
          sha256: object.customMetadata.sha256,
          size: object.size,
          suffix,
        });
        selectedBytes += object.size;
      }
    items.sort((left, right) => left.suffix.localeCompare(right.suffix));
    return {
      items,
      lastKey: items.length === 0 ? undefined : `${prefix}${items.at(-1)!.suffix}`,
      truncated: page.truncated || items.length < page.objects.length,
    };
  }

  #assertCommittedIdentity(progress: TransferProgress): void {
    const state = this.#state();
    if (progress.archived_bytes !== state.archived_bytes
      || progress.archived_receipts !== state.archived_receipts
      || progress.object_count !== state.object_count) {
      throw new Error("managed turn archive objects do not match committed state");
    }
  }

  async #read(key: string): Promise<ManagedTurnReceipt | undefined> {
    const object = await this.#bucket.get(key);
    if (!object) return undefined;
    if (!object.body) {
      throw new Error("managed turn archive receipt body is unavailable");
    }
    const body = new Uint8Array(await object.arrayBuffer());
    const expectedHash = object.customMetadata?.sha256;
    if (!expectedHash || object.customMetadata?.kind !== "managed_turn_receipt"
      || object.customMetadata?.version !== String(VERSION)
      || await sha256Hex(body) !== expectedHash) {
      throw new Error("managed turn archive receipt checksum mismatch");
    }
    const envelope = JSON.parse(new TextDecoder().decode(body)) as ReceiptEnvelope;
    if (envelope.version !== VERSION || envelope.kind !== "managed_turn_receipt") {
      throw new Error("managed turn archive receipt envelope is invalid");
    }
    validateReceipt(envelope.receipt);
    return envelope.receipt;
  }

  async #putImmutable(
    key: string,
    body: Uint8Array,
    sha256: string,
    assertOwnership: () => void = () => {},
  ): Promise<void> {
    assertOwnership();
    const stored = await this.#bucket.put(key, body, {
      onlyIf: { etagDoesNotMatch: "*" },
      httpMetadata: { contentType: "application/json" },
      customMetadata: { kind: "managed_turn_receipt", sha256, version: String(VERSION) },
      sha256,
    });
    assertOwnership();
    if (stored) return;
    const existing = await this.#bucket.head(key);
    assertOwnership();
    if (!existing || existing.size !== body.byteLength
      || existing.customMetadata?.sha256 !== sha256
      || existing.customMetadata?.kind !== "managed_turn_receipt") {
      throw new Error("managed turn archive immutable receipt conflicts with existing data");
    }
  }

  #terminalCount(): number {
    return this.#storage.sql.exec<{ rows: number }>(
      "SELECT COUNT(*) AS rows FROM managed_turns WHERE state IN ('completed', 'cancelled', 'failed')",
    ).toArray()[0]?.rows ?? 0;
  }

  #state(): ArchiveState {
    const state = this.#storage.sql.exec<ArchiveState>(
      `SELECT archived_receipts, archived_bytes, object_count
       FROM managed_turn_archive_state WHERE singleton = 1`,
    ).toArray()[0];
    if (!state) throw new Error("managed turn archive state is unavailable");
    return state;
  }
}

const RECEIPT_SELECT = `
  SELECT id, request_key, request_hash, input_json, state,
         CAST(accepted_cursor AS TEXT) AS accepted_cursor,
         terminal_json, CAST(terminal_cursor AS TEXT) AS terminal_cursor,
         error, may_have_inner_operation, attempt_count, retry_at,
         created_at, accepted_at, updated_at
  FROM managed_turns`;

function validateReceipt(value: ManagedTurnReceipt): void {
  if (!value || typeof value !== "object"
    || typeof value.id !== "string" || value.id.length === 0
    || value.id.length > 128
    || typeof value.request_hash !== "string" || !/^[0-9a-f]{64}$/.test(value.request_hash)
    || typeof value.input_json !== "string"
    || (value.request_key !== null
      && (typeof value.request_key !== "string" || value.request_key.length === 0))
    || !["completed", "cancelled", "failed"].includes(value.state)
    || typeof value.terminal_json !== "string" || value.terminal_json.length === 0
    || typeof value.accepted_cursor !== "string" || !/^[1-9][0-9]*$/.test(value.accepted_cursor)
    || typeof value.terminal_cursor !== "string" || !/^[1-9][0-9]*$/.test(value.terminal_cursor)
    || BigInt(value.accepted_cursor) > BigInt(value.terminal_cursor)
    || (value.error !== null && typeof value.error !== "string")
    || (value.may_have_inner_operation !== 0 && value.may_have_inner_operation !== 1)
    || value.retry_at !== null
    || !Number.isSafeInteger(value.created_at) || value.created_at < 0
    || !Number.isSafeInteger(value.accepted_at) || value.accepted_at < value.created_at
    || !Number.isSafeInteger(value.updated_at) || value.updated_at < value.accepted_at
    || !Number.isSafeInteger(value.attempt_count) || value.attempt_count < 0) {
    throw new Error("managed turn archive receipt is invalid");
  }
  JSON.parse(value.input_json);
  const terminal = JSON.parse(value.terminal_json) as { id?: unknown; type?: unknown };
  const expectedType = `turn_${value.state}`;
  if (!terminal || typeof terminal !== "object"
    || terminal.id !== value.id || terminal.type !== expectedType) {
    throw new Error("managed turn archive terminal projection is invalid");
  }
}

function validateIdentity(value: ManagedTurnArchiveIdentity): void {
  if (!value || typeof value !== "object"
    || value.version !== VERSION
    || !Number.isSafeInteger(value.archived_bytes) || value.archived_bytes < 0
    || !Number.isSafeInteger(value.archived_receipts) || value.archived_receipts < 0
    || !Number.isSafeInteger(value.objects) || value.objects < 0
    || value.archived_receipts > value.objects
    || typeof value.digest !== "string" || !/^[0-9a-f]{64}$/.test(value.digest)) {
    throw new Error("managed turn archive identity is invalid");
  }
}

function emptySeal(): ManagedTurnSealResult {
  return { archived_bytes: 0, archived_receipts: 0, objects: 0, sealed: false };
}

async function advanceProgress(
  progress: TransferProgress,
  items: readonly ArchivedObject[],
): Promise<TransferProgress> {
  const digest = items.length === 0
    ? progress.digest
    : await sha256Hex(encoder.encode(`${progress.digest}\n${JSON.stringify(items)}`));
  return {
    archived_bytes: progress.archived_bytes
      + items.reduce((sum, item) => sum + item.size, 0),
    archived_receipts: progress.archived_receipts
      + items.filter(({ suffix }) => suffix.startsWith("by-id/")).length,
    complete: 0,
    digest,
    last_key: progress.last_key,
    object_count: progress.object_count + items.length,
  };
}

function identityFromProgress(progress: TransferProgress): ManagedTurnArchiveIdentity {
  return {
    archived_bytes: progress.archived_bytes,
    archived_receipts: progress.archived_receipts,
    digest: progress.digest,
    objects: progress.object_count,
    version: VERSION,
  };
}

async function mapWithConcurrency<T>(
  values: readonly T[],
  concurrency: number,
  operation: (value: T) => Promise<void>,
): Promise<void> {
  let next = 0;
  const settled = await Promise.allSettled(Array.from(
    { length: Math.min(concurrency, values.length) },
    async () => {
      while (next < values.length) {
        const index = next;
        next += 1;
        await operation(values[index]!);
      }
    },
  ));
  const failure = settled.find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (failure) throw failure.reason;
}
