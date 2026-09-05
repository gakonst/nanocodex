import { describe, expect, it } from "vitest";

import { sha256Hex } from "../src/archive-hash";
import { ManagedPortabilityArchive } from "../src/managed-portability-archive";

const SOURCE_STORAGE_ID = "a".repeat(64);
const DESTINATION_STORAGE_ID = "b".repeat(64);

describe("managed portability archive", () => {
  it("exports and adopts a live event segment larger than 16 MiB", async () => {
    const bucket = new MemoryR2Bucket();
    // Portability validates the immutable object's content-addressed identity;
    // the event archive owns its JSON envelope. Avoid constructing several
    // giant intermediate strings in the Workers test runtime.
    const body = new Uint8Array(16 * 1024 * 1024 + 1);
    expect(body.byteLength).toBeGreaterThan(16 * 1024 * 1024);
    const sha256 = await sha256Hex(body);
    const suffix = `segments/${"1".padStart(19, "0")}-${"1".padStart(19, "0")}-${sha256}.json`;
    bucket.seed(`agents/${SOURCE_STORAGE_ID}/managed-events/${suffix}`, body, {
      kind: "managed_event_segment",
      sha256,
      version: "1",
    });

    const source = new ManagedPortabilityArchive(
      new PortabilityStorage() as unknown as DurableObjectStorage,
      bucket as unknown as R2Bucket,
      SOURCE_STORAGE_ID,
    );
    const sourceBatch = await source.identityBatch("events");
    expect(sourceBatch).toMatchObject({
      complete: true,
      identity: { bytes: body.byteLength, objects: 1, version: 1 },
      objects: 1,
    });

    const destination = new ManagedPortabilityArchive(
      new PortabilityStorage() as unknown as DurableObjectStorage,
      bucket as unknown as R2Bucket,
      DESTINATION_STORAGE_ID,
    );
    const adopted = await destination.adoptBatch(
      "events",
      SOURCE_STORAGE_ID,
      sourceBatch.identity!,
      () => {},
    );
    expect(adopted).toEqual({
      complete: true,
      identity: sourceBatch.identity,
      objects: 1,
    });
    await expect(destination.identityBatch("events")).resolves.toEqual({
      complete: true,
      identity: sourceBatch.identity,
      objects: 1,
    });
    const copied = bucket.bytes(`agents/${DESTINATION_STORAGE_ID}/managed-events/${suffix}`)!;
    expect(copied.byteLength).toBe(body.byteLength);
    await expect(sha256Hex(copied)).resolves.toBe(sha256);
  });
});

type Progress = {
  bytes: number;
  complete: number;
  digest: string;
  last_key: string | null;
  objects: number;
};

type AdoptionProgress = Progress & {
  manifest_digest: string;
  source_storage_id: string;
};

class PortabilityStorage {
  readonly #adoptions = new Map<string, AdoptionProgress>();
  readonly #manifests = new Map<string, Progress>();

  readonly sql = {
    exec: <Row extends Record<string, SqlStorageValue>>(
      query: string,
      ...bindings: SqlStorageValue[]
    ): SqlStorageCursor<Row> => {
      const normalized = query.replace(/\s+/g, " ").trim();
      if (normalized.startsWith("CREATE TABLE")) return cursor<Row>([]);
      if (normalized.startsWith("SELECT last_key, digest")) {
        return cursor<Row>(optionalRow(this.#manifests.get(String(bindings[0]))));
      }
      if (normalized.startsWith("INSERT INTO managed_portability_manifest_progress")) {
        this.#manifests.set(String(bindings[0]), emptyProgress());
        return cursor<Row>([]);
      }
      if (normalized.startsWith("UPDATE managed_portability_manifest_progress")) {
        this.#manifests.set(String(bindings[5]), {
          last_key: bindings[0] as string | null,
          digest: String(bindings[1]),
          bytes: Number(bindings[2]),
          objects: Number(bindings[3]),
          complete: Number(bindings[4]),
        });
        return cursor<Row>([]);
      }
      if (normalized.startsWith("SELECT source_storage_id, manifest_digest")) {
        return cursor<Row>(optionalRow(this.#adoptions.get(String(bindings[0]))));
      }
      if (normalized.startsWith("INSERT INTO managed_portability_adoption_progress")) {
        this.#adoptions.set(String(bindings[0]), {
          ...emptyProgress(),
          source_storage_id: String(bindings[1]),
          manifest_digest: String(bindings[2]),
        });
        return cursor<Row>([]);
      }
      if (normalized.startsWith("UPDATE managed_portability_adoption_progress")) {
        const kind = String(bindings[5]);
        const retained = this.#adoptions.get(kind)!;
        this.#adoptions.set(kind, {
          ...retained,
          last_key: bindings[0] as string | null,
          digest: String(bindings[1]),
          bytes: Number(bindings[2]),
          objects: Number(bindings[3]),
          complete: Number(bindings[4]),
        });
        return cursor<Row>([]);
      }
      throw new Error(`unexpected SQL: ${normalized}`);
    },
  };

  transactionSync<Result>(callback: () => Result): Result {
    return callback();
  }
}

type StoredObject = {
  body: Uint8Array;
  customMetadata: Record<string, string>;
};

class MemoryR2Bucket {
  readonly #objects = new Map<string, StoredObject>();

  seed(key: string, body: Uint8Array, customMetadata: Record<string, string>): void {
    this.#objects.set(key, { body: body.slice(), customMetadata: { ...customMetadata } });
  }

  bytes(key: string): Uint8Array | undefined {
    return this.#objects.get(key)?.body;
  }

  async list(options: R2ListOptions): Promise<R2Objects> {
    const keys = [...this.#objects.keys()]
      .filter((key) => key.startsWith(options.prefix ?? "")
        && (options.startAfter === undefined || key > options.startAfter))
      .sort()
      .slice(0, options.limit);
    return {
      delimitedPrefixes: [],
      objects: keys.map((key) => this.object(key)),
      truncated: false,
    } as R2Objects;
  }

  async get(key: string): Promise<R2ObjectBody | null> {
    const stored = this.#objects.get(key);
    if (!stored) return null;
    return {
      ...this.object(key),
      arrayBuffer: async () => stored.body.slice().buffer,
      body: new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(stored.body.slice());
          controller.close();
        },
      }),
      blob: async () => new Blob([stored.body]),
      json: async () => JSON.parse(new TextDecoder().decode(stored.body)),
      text: async () => new TextDecoder().decode(stored.body),
      writeHttpMetadata: () => {},
    } as unknown as R2ObjectBody;
  }

  async put(
    key: string,
    value: ReadableStream | ArrayBuffer | ArrayBufferView | string | Blob,
    options?: R2PutOptions,
  ): Promise<R2Object | null> {
    if (this.#objects.has(key) && options?.onlyIf) return null;
    const body = value instanceof Uint8Array
      ? value.slice()
      : new Uint8Array(await new Response(value as BodyInit).arrayBuffer());
    this.seed(key, body, options?.customMetadata ?? {});
    return this.object(key);
  }

  async head(key: string): Promise<R2Object | null> {
    return this.#objects.has(key) ? this.object(key) : null;
  }

  private object(key: string): R2Object {
    const stored = this.#objects.get(key)!;
    return {
      checksums: {},
      customMetadata: { ...stored.customMetadata },
      etag: stored.customMetadata.sha256 ?? "etag",
      httpEtag: `\"${stored.customMetadata.sha256 ?? "etag"}\"`,
      key,
      size: stored.body.byteLength,
      uploaded: new Date(0),
      version: "test",
      writeHttpMetadata: () => {},
    } as unknown as R2Object;
  }
}

function emptyProgress(): Progress {
  return {
    bytes: 0,
    complete: 0,
    digest: "0".repeat(64),
    last_key: null,
    objects: 0,
  };
}

function optionalRow<Row extends object>(value: Row | undefined): Row[] {
  return value === undefined ? [] : [{ ...value }];
}

function cursor<Row extends Record<string, SqlStorageValue>>(rows: object[]): SqlStorageCursor<Row> {
  return {
    columnNames: [],
    one: () => {
      if (rows.length !== 1) throw new Error(`expected one row, received ${rows.length}`);
      return rows[0] as Row;
    },
    raw: function* () {},
    rowsRead: rows.length,
    rowsWritten: 0,
    toArray: () => rows.map((row) => ({ ...row })) as Row[],
    [Symbol.iterator]: function* () {
      yield* rows as Row[];
    },
  } as unknown as SqlStorageCursor<Row>;
}
