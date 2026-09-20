import { createHash } from "node:crypto";

// Storage placement, not a file-size limit: larger bodies keep streaming to R2.
// SQLite's maximum row size is 2 MB, so leave room for the key and metadata.
export const BRAIN_INLINE_BYTES = 1024 * 1024;
type Row = { key: string; metadata: string; body: ArrayBuffer | null };
type Metadata = {
  key: string; version: string; size: number; etag: string; uploaded: number;
  httpMetadata: R2HTTPMetadata; customMetadata: Record<string, string>;
};

/** Agent-owned file metadata and small bodies stay beside the executing brain. */
export function createBrainBucket(storage: DurableObjectStorage, backing: R2Bucket, resourceId: string): R2Bucket {
  if (!/^[A-Za-z0-9._:-]{1,256}$/.test(resourceId)) throw new TypeError("invalid brain resource id");
  const prefix = `brains/${resourceId}/`;
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS nanocodex_brain_objects (
    key TEXT PRIMARY KEY, metadata TEXT NOT NULL, body BLOB
  )`);
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS nanocodex_brain_catalog (
    prefix TEXT PRIMARY KEY
  )`);
  const validate = (key: string): void => {
    if (!key.startsWith(prefix) || key.includes("\0")
      || key.slice(prefix.length).split("/").some((part) => part === "." || part === "..")) {
      throw new Error("brain object is outside its owning agent");
    }
  };
  const retain = (object: R2Object, body: ArrayBuffer | null): R2Object => {
    const metadata: Metadata = {
      key: object.key, version: object.version, size: object.size, etag: object.etag,
      uploaded: object.uploaded.getTime(), httpMetadata: object.httpMetadata ?? {},
      customMetadata: object.customMetadata ?? {},
    };
    storage.sql.exec("INSERT OR REPLACE INTO nanocodex_brain_objects VALUES (?, ?, ?)",
      object.key, JSON.stringify(metadata), body);
    return object;
  };
  let opening: Promise<void> | undefined;
  const ready = (): Promise<void> => opening ??= (async () => {
    if (storage.sql.exec("SELECT prefix FROM nanocodex_brain_catalog WHERE prefix = ?", prefix).toArray().length) return;
    let cursor: string | undefined;
    do {
      const page = await backing.list({ prefix, cursor, include: ["httpMetadata", "customMetadata"] });
      for (const object of page.objects) retain(object, null);
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor !== undefined);
    storage.sql.exec("INSERT OR IGNORE INTO nanocodex_brain_catalog VALUES (?)", prefix);
  })().catch((error) => { opening = undefined; throw error; });
  const row = (key: string): Row | undefined => storage.sql.exec<Row>(
    "SELECT key, metadata, body FROM nanocodex_brain_objects WHERE key = ?", key,
  ).toArray()[0];
  const head = async (key: string): Promise<R2Object | null> => {
    validate(key); await ready();
    const found = storage.sql.exec<{ metadata: string }>(
      "SELECT metadata FROM nanocodex_brain_objects WHERE key = ?", key,
    ).toArray()[0];
    return found ? objectMetadata(JSON.parse(found.metadata)) : null;
  };
  const multipart = (upload: R2MultipartUpload): R2MultipartUpload => ({
    key: upload.key, uploadId: upload.uploadId,
    uploadPart: (number, value, options) => upload.uploadPart(number, value, options),
    abort: () => upload.abort(),
    async complete(parts) {
      await ready();
      return retain(await upload.complete(parts), null);
    },
  });
  return {
    head,
    async get(key: string, options: R2GetOptions = {}) {
      validate(key); await ready();
      const found = row(key);
      if (!found) return null;
      if (found.body === null) return backing.get(key, options);
      const object = objectMetadata(JSON.parse(found.metadata));
      if (!conditionMatches(object, options.onlyIf)) return object;
      let bytes = new Uint8Array(found.body);
      const range = options.range instanceof Headers ? undefined : options.range;
      let selected: { offset: number; length: number } | undefined;
      if (range) {
        const offset = "suffix" in range ? Math.max(0, bytes.length - range.suffix) : range.offset ?? 0;
        const length = "suffix" in range ? bytes.length - offset : Math.min(range.length ?? bytes.length, bytes.length - offset);
        if (offset < 0 || length < 0 || offset >= bytes.length && bytes.length !== 0) throw new RangeError("invalid object range");
        bytes = bytes.slice(offset, offset + length);
        selected = { offset, length };
      }
      const response = new Response(bytes);
      return {
        ...object, ...(selected ? { range: selected } : {}), body: response.body!,
        get bodyUsed() { return response.bodyUsed; },
        arrayBuffer: () => response.arrayBuffer(), text: () => response.text(),
        json: <T>() => response.json<T>(), blob: () => response.blob(),
      } as R2ObjectBody;
    },
    async put(key: string, value: ReadableStream | ArrayBuffer | ArrayBufferView | string | Blob | null,
      options: R2PutOptions = {}) {
      validate(key); await ready();
      if (options.onlyIf && !conditionMatches(await head(key), options.onlyIf)) return null;
      const size = typeof value === "string" ? new TextEncoder().encode(value).byteLength
        : value instanceof Blob ? value.size
        : value instanceof ArrayBuffer || ArrayBuffer.isView(value) ? value.byteLength : 0;
      // Forward streams intact: R2 needs the runtime's known-length body, and
      // attachments/native multipart uploads must never be buffered here.
      if (value instanceof ReadableStream || size > BRAIN_INLINE_BYTES) {
        const object = await backing.put(key, value, options);
        return object ? retain(object, null) : null;
      }
      const bytes = new Uint8Array(await new Response(value as BodyInit | null).arrayBuffer());
      const object = objectMetadata({
        key, size, version: crypto.randomUUID(), etag: createHash("md5").update(bytes).digest("hex"),
        uploaded: Date.now(), httpMetadata: httpMetadata(options.httpMetadata),
        customMetadata: options.customMetadata ?? {},
      });
      return retain(object, bytes.buffer);
    },
    async delete(keys: string | string[]) {
      const selected = typeof keys === "string" ? [keys] : keys;
      selected.forEach(validate); await ready();
      // Also remove any older R2 version shadowed by an inline replacement.
      if (selected.length) await backing.delete(selected);
      storage.transactionSync(() => {
        for (const key of selected) storage.sql.exec("DELETE FROM nanocodex_brain_objects WHERE key = ?", key);
      });
    },
    async list(options: R2ListOptions = {}) {
      const requested = options.prefix ?? prefix;
      if (!requested.startsWith(prefix)) throw new Error("brain listing is outside its owning agent");
      await ready();
      const objects: R2Object[] = [];
      const delimitedPrefixes: string[] = [];
      const candidates = new Map<string, R2Object | null>();
      const after = options.cursor ? decodeURIComponent(options.cursor) : options.startAfter ?? "";
      const limit = options.limit ?? 1000;
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1000) throw new RangeError("invalid R2 listing limit");
      let truncated = false;
      for (const found of storage.sql.exec<{ key: string; metadata: string }>(
        "SELECT key, metadata FROM nanocodex_brain_objects WHERE key >= ? AND key < ? AND key > ? ORDER BY key",
        requested, `${requested}\uffff`, after,
      )) {
        if (!found.key.startsWith(requested)) continue;
        const delimiterAt = options.delimiter ? found.key.indexOf(options.delimiter, requested.length) : -1;
        const key = delimiterAt !== -1 ? found.key.slice(0, delimiterAt + options.delimiter!.length) : found.key;
        if (key <= after || candidates.has(key)) continue;
        if (candidates.size === limit) { truncated = true; break; }
        candidates.set(key, delimiterAt !== -1 ? null : objectMetadata(JSON.parse(found.metadata)));
      }
      const entries = [...candidates];
      for (const [key, object] of entries) {
        if (object) objects.push(object); else delimitedPrefixes.push(key);
      }
      return { objects, delimitedPrefixes, truncated,
        ...(truncated ? { cursor: encodeURIComponent(entries[limit - 1]![0]) } : {}) };
    },
    async createMultipartUpload(key: string, options?: R2MultipartOptions) {
      validate(key); await ready();
      return multipart(await backing.createMultipartUpload(key, options));
    },
    resumeMultipartUpload(key: string, uploadId: string) {
      validate(key);
      return multipart(backing.resumeMultipartUpload(key, uploadId));
    },
  } as R2Bucket;
}

function httpMetadata(value: R2HTTPMetadata | Headers | undefined): R2HTTPMetadata {
  if (!(value instanceof Headers)) return value ?? {};
  return Object.fromEntries([
    ["contentType", "content-type"], ["contentLanguage", "content-language"],
    ["contentDisposition", "content-disposition"], ["contentEncoding", "content-encoding"],
    ["cacheControl", "cache-control"],
  ].flatMap(([field, header]) => value.has(header!) ? [[field!, value.get(header!)!]] : []));
}

function objectMetadata(metadata: Metadata): R2Object {
  if (metadata.httpMetadata.cacheExpiry) metadata.httpMetadata.cacheExpiry = new Date(metadata.httpMetadata.cacheExpiry);
  const object = {
    ...metadata, uploaded: new Date(metadata.uploaded), httpEtag: `"${metadata.etag}"`,
    checksums: { toJSON: () => ({}) }, storageClass: "Standard",
    writeHttpMetadata(headers: Headers) {
      for (const [field, header] of [
        ["contentType", "content-type"], ["contentLanguage", "content-language"],
        ["contentDisposition", "content-disposition"], ["contentEncoding", "content-encoding"],
        ["cacheControl", "cache-control"],
      ] as const) {
        const value = metadata.httpMetadata[field];
        if (value !== undefined) headers.set(header, value);
      }
      if (metadata.httpMetadata.cacheExpiry) headers.set("expires", new Date(metadata.httpMetadata.cacheExpiry).toUTCString());
    },
  };
  return object as R2Object;
}

function conditionMatches(object: R2Object | null, condition?: R2Conditional | Headers): boolean {
  if (!condition) return true;
  const value = condition instanceof Headers ? {
    etagMatches: condition.get("if-match") ?? undefined,
    etagDoesNotMatch: condition.get("if-none-match") ?? undefined,
    uploadedBefore: condition.has("if-unmodified-since") ? new Date(condition.get("if-unmodified-since")!) : undefined,
    uploadedAfter: condition.has("if-modified-since") ? new Date(condition.get("if-modified-since")!) : undefined,
  } : condition;
  const matches = (etag: string) => etag === "*" ? object !== null : object?.etag === etag.replace(/^"|"$/g, "");
  return (!value.etagMatches || matches(value.etagMatches))
    && (!value.etagDoesNotMatch || !matches(value.etagDoesNotMatch))
    && (!value.uploadedBefore || object !== null && object.uploaded <= value.uploadedBefore)
    && (!value.uploadedAfter || object !== null && object.uploaded > value.uploadedAfter);
}
