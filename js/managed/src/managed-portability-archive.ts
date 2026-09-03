import { sha256Hex } from "./archive-hash";

const VERSION = 1;
const BATCH_OBJECTS = 64;
const BATCH_BYTES = 8 * 1024 * 1024;
const COPY_CONCURRENCY = 4;
const EMPTY_DIGEST = "0".repeat(64);
const encoder = new TextEncoder();

export type ManagedPortableArchiveKind = "events" | "realtime";

export type ManagedPortableArchiveIdentity = Readonly<{
  bytes: number;
  digest: string;
  objects: number;
  version: 1;
}>;

export type ManagedPortableArchiveBatch = Readonly<{
  complete: boolean;
  identity?: ManagedPortableArchiveIdentity;
  objects: number;
}>;

type Progress = {
  bytes: number;
  complete: number;
  digest: string;
  last_key: string | null;
  objects: number;
};

type PortableObject = Readonly<{
  kind: string;
  sha256: string;
  size: number;
  suffix: string;
}>;

/** Bounded immutable R2 transfer for managed event and realtime archives. */
export class ManagedPortabilityArchive {
  readonly #bucket: R2Bucket;
  readonly #storageId: string;
  readonly #storage: DurableObjectStorage;

  constructor(storage: DurableObjectStorage, bucket: R2Bucket, storageId: string) {
    this.#bucket = bucket;
    this.#storageId = storageId;
    this.#storage = storage;
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_portability_manifest_progress (
        kind TEXT PRIMARY KEY CHECK (kind IN ('events', 'realtime')),
        last_key TEXT,
        digest TEXT NOT NULL,
        bytes INTEGER NOT NULL DEFAULT 0,
        objects INTEGER NOT NULL DEFAULT 0,
        complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1))
      );
      CREATE TABLE IF NOT EXISTS managed_portability_adoption_progress (
        kind TEXT PRIMARY KEY CHECK (kind IN ('events', 'realtime')),
        source_storage_id TEXT NOT NULL,
        manifest_digest TEXT NOT NULL,
        last_key TEXT,
        scan_digest TEXT NOT NULL,
        bytes INTEGER NOT NULL DEFAULT 0,
        objects INTEGER NOT NULL DEFAULT 0,
        complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1))
      );
    `);
  }

  async identityBatch(kind: ManagedPortableArchiveKind): Promise<ManagedPortableArchiveBatch> {
    let progress = this.#storage.sql.exec<Progress>(
      `SELECT last_key, digest, bytes, objects, complete
       FROM managed_portability_manifest_progress WHERE kind = ?`,
      kind,
    ).toArray()[0];
    if (!progress) {
      this.#storage.sql.exec(
        `INSERT INTO managed_portability_manifest_progress (kind, digest)
         VALUES (?, ?)`,
        kind,
        EMPTY_DIGEST,
      );
      progress = emptyProgress();
    }
    if (progress.complete === 1) {
      return { complete: true, identity: identity(progress), objects: 0 };
    }
    const page = await this.#page(kind, this.#storageId, progress.last_key);
    const next = await advance(progress, page.items);
    const complete = !page.truncated;
    this.#storage.sql.exec(
      `UPDATE managed_portability_manifest_progress
       SET last_key = ?, digest = ?, bytes = ?, objects = ?, complete = ?
       WHERE kind = ?`,
      page.lastKey ?? progress.last_key,
      next.digest,
      next.bytes,
      next.objects,
      complete ? 1 : 0,
      kind,
    );
    return {
      complete,
      ...(complete ? { identity: identity(next) } : {}),
      objects: page.items.length,
    };
  }

  async adoptBatch(
    kind: ManagedPortableArchiveKind,
    sourceStorageId: string,
    expected: ManagedPortableArchiveIdentity,
    assertOwnership: () => void,
  ): Promise<ManagedPortableArchiveBatch> {
    assertOwnership();
    if (!/^[0-9a-f]{64}$/.test(sourceStorageId) || sourceStorageId === this.#storageId) {
      throw new Error(`managed ${kind} archive adoption source is invalid`);
    }
    validateIdentity(expected);
    let progress = this.#storage.sql.exec<Progress & {
      manifest_digest: string;
      source_storage_id: string;
    }>(
      `SELECT source_storage_id, manifest_digest, last_key, scan_digest AS digest,
              bytes, objects, complete
       FROM managed_portability_adoption_progress WHERE kind = ?`,
      kind,
    ).toArray()[0];
    if (progress) {
      if (progress.source_storage_id !== sourceStorageId
        || progress.manifest_digest !== expected.digest) {
        throw new Error(`managed ${kind} archive adoption conflicts with retained identity`);
      }
      if (progress.complete === 1) {
        return { complete: true, identity: expected, objects: 0 };
      }
    } else {
      this.#storage.sql.exec(
        `INSERT INTO managed_portability_adoption_progress (
           kind, source_storage_id, manifest_digest, scan_digest
         ) VALUES (?, ?, ?, ?)`,
        kind,
        sourceStorageId,
        expected.digest,
        EMPTY_DIGEST,
      );
      progress = {
        ...emptyProgress(),
        manifest_digest: expected.digest,
        source_storage_id: sourceStorageId,
      };
    }
    const page = await this.#page(kind, sourceStorageId, progress.last_key, assertOwnership);
    assertOwnership();
    await mapWithConcurrency(page.items, COPY_CONCURRENCY, async (item) => {
      assertOwnership();
      const sourceKey = `${prefix(sourceStorageId, kind)}${item.suffix}`;
      const source = await this.#bucket.get(sourceKey);
      assertOwnership();
      if (!source?.body || source.size !== item.size
        || source.customMetadata?.kind !== item.kind
        || source.customMetadata?.version !== String(VERSION)
        || source.customMetadata?.sha256 !== item.sha256) {
        await source?.body?.cancel();
        throw new Error(`managed ${kind} archive source object is unavailable`);
      }
      const body = new Uint8Array(await source.arrayBuffer());
      assertOwnership();
      if (await sha256Hex(body) !== item.sha256) {
        throw new Error(`managed ${kind} archive source checksum mismatch`);
      }
      assertOwnership();
      const destinationKey = `${prefix(this.#storageId, kind)}${item.suffix}`;
      const stored = await this.#bucket.put(destinationKey, body, {
        onlyIf: { etagDoesNotMatch: "*" },
        httpMetadata: { contentType: "application/json" },
        customMetadata: { kind: item.kind, sha256: item.sha256, version: String(VERSION) },
        sha256: item.sha256,
      });
      assertOwnership();
      if (stored) return;
      const retained = await this.#bucket.head(destinationKey);
      assertOwnership();
      if (!retained || retained.size !== item.size
        || retained.customMetadata?.kind !== item.kind
        || retained.customMetadata?.sha256 !== item.sha256) {
        throw new Error(`managed ${kind} archive immutable destination conflicts`);
      }
    });
    assertOwnership();
    const next = await advance(progress, page.items);
    assertOwnership();
    const complete = !page.truncated;
    if (complete && JSON.stringify(identity(next)) !== JSON.stringify(expected)) {
      throw new Error(`managed ${kind} archive source identity changed during adoption`);
    }
    this.#storage.transactionSync(() => {
      assertOwnership();
      this.#storage.sql.exec(
        `UPDATE managed_portability_adoption_progress
         SET last_key = ?, scan_digest = ?, bytes = ?, objects = ?, complete = ?
         WHERE kind = ?`,
        page.lastKey ?? progress.last_key,
        next.digest,
        next.bytes,
        next.objects,
        complete ? 1 : 0,
        kind,
      );
    });
    return {
      complete,
      ...(complete ? { identity: expected } : {}),
      objects: page.items.length,
    };
  }

  clearLocalState(): void {
    this.#storage.sql.exec(`
      DELETE FROM managed_portability_manifest_progress;
      DELETE FROM managed_portability_adoption_progress;
    `);
  }

  async #page(
    kind: ManagedPortableArchiveKind,
    storageId: string,
    lastKey: string | null,
    assertOwnership: () => void = () => {},
  ): Promise<{ items: PortableObject[]; lastKey?: string; truncated: boolean }> {
    assertOwnership();
    const archivePrefix = prefix(storageId, kind);
    const page = await this.#bucket.list({
      prefix: archivePrefix,
      limit: BATCH_OBJECTS,
      ...(lastKey === null ? {} : { startAfter: lastKey }),
      include: ["customMetadata"],
    });
    assertOwnership();
    const items: PortableObject[] = [];
    let bytes = 0;
    for (const object of page.objects) {
      const suffix = object.key.slice(archivePrefix.length);
      const objectKind = object.customMetadata?.kind ?? "";
      if (!validObject(kind, suffix, objectKind)
        || object.size <= 0
        || object.customMetadata?.version !== String(VERSION)
        || !/^[0-9a-f]{64}$/.test(object.customMetadata?.sha256 ?? "")) {
        throw new Error(`managed ${kind} archive contains an invalid object identity`);
      }
      if (items.length > 0 && bytes + object.size > BATCH_BYTES) break;
      items.push({
        kind: objectKind,
        sha256: object.customMetadata!.sha256!,
        size: object.size,
        suffix,
      });
      bytes += object.size;
    }
    items.sort((left, right) => left.suffix.localeCompare(right.suffix));
    return {
      items,
      lastKey: items.length === 0
        ? undefined
        : `${archivePrefix}${items.at(-1)!.suffix}`,
      truncated: page.truncated || items.length < page.objects.length,
    };
  }
}

function prefix(storageId: string, kind: ManagedPortableArchiveKind): string {
  return `agents/${storageId}/managed-${kind}/`;
}

function validObject(kind: ManagedPortableArchiveKind, suffix: string, objectKind: string): boolean {
  if (kind === "realtime") {
    return /^by-id\/[0-9a-f]{64}\.json$/.test(suffix)
      && objectKind === "managed_realtime_receipt";
  }
  return (/^segments\/[0-9]{19}-[0-9]{19}-[0-9a-f]{64}\.json$/.test(suffix)
      && objectKind === "managed_event_segment")
    || (/^indexes\/[0-9]{16}\.json$/.test(suffix)
      && objectKind === "managed_event_index");
}

function emptyProgress(): Progress {
  return { bytes: 0, complete: 0, digest: EMPTY_DIGEST, last_key: null, objects: 0 };
}

async function advance(progress: Progress, items: readonly PortableObject[]): Promise<Progress> {
  return {
    bytes: progress.bytes + items.reduce((sum, item) => sum + item.size, 0),
    complete: 0,
    digest: items.length === 0
      ? progress.digest
      : await sha256Hex(encoder.encode(`${progress.digest}\n${JSON.stringify(items)}`)),
    last_key: progress.last_key,
    objects: progress.objects + items.length,
  };
}

function identity(progress: Progress): ManagedPortableArchiveIdentity {
  return { bytes: progress.bytes, digest: progress.digest, objects: progress.objects, version: 1 };
}

function validateIdentity(value: ManagedPortableArchiveIdentity): void {
  if (!value || value.version !== VERSION
    || !Number.isSafeInteger(value.bytes) || value.bytes < 0
    || !Number.isSafeInteger(value.objects) || value.objects < 0
    || !/^[0-9a-f]{64}$/.test(value.digest)) {
    throw new Error("managed portable archive identity is invalid");
  }
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
