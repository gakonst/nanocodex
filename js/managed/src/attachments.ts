import type { AttachmentUploadSigner } from "./attachment-r2";
// Original attachment bytes live in the same /brain filesystem mounted by Hands.
// The client sends paths to the agent; media decoding belongs to its tools.
export const ATTACHMENT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const ATTACHMENT_PART_BYTES = 8 * 1024 * 1024;
// Workers Free/Pro ingress is 100 MB; R2 permits 10,000 parts. Scale the
// transfer size only when needed, up to those platform limits.
// https://developers.cloudflare.com/workers/platform/limits/#request-and-response-limits
// https://developers.cloudflare.com/r2/platform/limits/
export const ATTACHMENT_MAX_REQUEST_BYTES = 100_000_000;
export const ATTACHMENT_MAX_BYTES = Math.min(5 * 1024 ** 4 - 5 * 1024 ** 3, ATTACHMENT_MAX_REQUEST_BYTES * 10_000);
export const attachmentPartSize = (size: number): number => Math.max(ATTACHMENT_PART_BYTES, Math.ceil(size / 10_000));
export const ATTACHMENT_DIRECT_MAX_BYTES = 5 * 1024 ** 4 - 5 * 1024 ** 3;
const PREFIX = "attachment:";

type Metadata = { name: string; media_type: string; size: number; transport?: "r2" };
type Intent = { size: number; md5: string };
type Preview = Intent & { key: string; uploadId: string; etag?: string; sealed?: boolean };
type Upload = Metadata & { path: string; key: string; uploadId: string; count: number; created: number; preview?: Preview };
type Part = R2UploadedPart & { sha256: string };

export class SessionAttachments {
  readonly #pending = new Map<string, Promise<Response>>();
  #ingesting: Promise<unknown> = Promise.resolve();
  readonly #readers = new Set<ReadableStreamDefaultReader<Uint8Array>>();
  #closed = false;
  constructor(
    private readonly storage: DurableObjectStorage,
    private readonly bucket: R2Bucket,
    private readonly sessionId: string,
    private readonly active: () => boolean,
    private readonly signer?: AttachmentUploadSigner,
  ) {}

  fetch(request: Request, id: string, action = ""): Promise<Response> {
    if (!ATTACHMENT_ID.test(id)) return Promise.resolve(reply({ error: "not_found" }, 404));
    const previous = this.#pending.get(id) ?? Promise.resolve();
    const run = async () => {
      try {
        this.#checkActive();
        return await this.#fetch(request, id, action);
      } catch (error) {
        if (error instanceof AttachmentFailure) return reply({ error: error.message }, error.status);
        throw error;
      }
    };
    const task = previous.catch(() => {}).then(() => {
      if (request.method !== "PUT" || !action.startsWith("parts/")) return run();
      // The service admits one part body at a time across attachment IDs. Calls
      // wait instead of multiplying buffers or imposing a concurrency rejection.
      const part = this.#ingesting.then(run, run);
      this.#ingesting = part.then(() => {}, () => {});
      return part;
    });
    this.#pending.set(id, task);
    void task.finally(() => { if (this.#pending.get(id) === task) this.#pending.delete(id); }).catch(() => {});
    return task;
  }

  async cleanup(): Promise<void> {
    this.#closed = true;
    await Promise.allSettled([...this.#readers].map((reader) => reader.cancel()));
    await Promise.allSettled(this.#pending.values());
    const uploads = await this.storage.list<Upload>({ prefix: PREFIX });
    for (const [key, upload] of uploads) {
      if (key.slice(PREFIX.length).includes(":")) continue;
      if (upload.preview && !await this.bucket.head(upload.preview.key)) await this.bucket.resumeMultipartUpload(upload.preview.key, upload.preview.uploadId).abort();
      if (!await this.bucket.head(upload.key)) await this.bucket.resumeMultipartUpload(upload.key, upload.uploadId).abort();
    }
    const keys = [...uploads.keys()];
    for (let offset = 0; offset < keys.length; offset += 128) await this.storage.delete(keys.slice(offset, offset + 128));
    // Completed files remain owned by the session's existing /brain cleanup.
  }

  #checkActive(): void {
    if (this.#closed || !this.active()) throw new AttachmentFailure(409, "agent_deleting");
  }

  async #partBody(request: Request, expected: number,
    consume?: (body: ReadableStream<Uint8Array>) => Promise<R2UploadedPart>,
  ): Promise<{ sha256: string; part?: R2UploadedPart }> {
    if (!request.body) throw new AttachmentFailure(400, "missing_body");
    const reader = request.body.getReader();
    this.#readers.add(reader);
    const digest = new crypto.DigestStream("SHA-256");
    void digest.digest.catch(() => {});
    const hash = digest.getWriter();
    const body = consume ? new FixedLengthStream(expected) : undefined;
    const writer = body?.writable.getWriter();
    let failure: unknown;
    const stored = consume && body ? consume(body.readable).catch(async (error) => {
      failure ??= error;
      await Promise.allSettled([writer!.abort(error), reader.cancel(error)]);
      throw error;
    }) : Promise.resolve(undefined);
    const pump = (async () => {
      let size = 0;
      try {
        while (true) {
          const { done, value } = await reader.read();
          this.#checkActive();
          if (done) break;
          size += value.byteLength;
          if (size > expected) throw new AttachmentFailure(413, "attachment_part_too_large");
          // Await both sinks per chunk; tee() would allow the faster sink to
          // buffer the entire part while the other sink waits on network I/O.
          await Promise.all([hash.write(value), writer?.write(value)]);
        }
        if (size !== expected) throw new AttachmentFailure(400, "attachment_part_size");
        await Promise.all([hash.close(), writer?.close()]);
      } catch (error) {
        failure ??= error;
        await Promise.allSettled([hash.abort(error), writer?.abort(error)]);
        throw error;
      }
    })();
    try {
      const [transfer, result] = await Promise.allSettled([pump, stored]);
      if (transfer.status === "rejected") throw failure ?? transfer.reason;
      if (result.status === "rejected") throw failure ?? result.reason;
      const part = result.value;
      this.#checkActive();
      const sha256 = Array.from(new Uint8Array(await digest.digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
      return { sha256, part };
    } finally {
      await reader.cancel().catch(() => {});
      this.#readers.delete(reader);
    }
  }

  async #sign(upload: { key: string; uploadId: string }, partNumber: number, intent: Intent) {
    if (!this.signer) throw new AttachmentFailure(503, "attachment_direct_upload_unavailable");
    const receipt = await this.signer.signPart({ key: upload.key, uploadId: upload.uploadId, partNumber, ...intent });
    this.#checkActive();
    return receipt;
  }

  async #fetch(request: Request, id: string, action: string): Promise<Response> {
    const storageKey = PREFIX + id;
    let upload = await this.storage.get<Upload>(storageKey);
    this.#checkActive();
    if (request.method === "POST" && action === "") {
      const value = await controlJSON(request, this.#readers, () => this.#checkActive());
      const metadata = parseMetadata(value);
      if (metadata.transport === "r2" && !this.signer) throw new AttachmentFailure(503, "attachment_direct_upload_unavailable");
      this.#checkActive();
      if (upload && (upload.name !== metadata.name || upload.media_type !== metadata.media_type || upload.size !== metadata.size || (upload.transport === "r2" && metadata.transport !== "r2"))) {
        throw new AttachmentFailure(409, "attachment_conflict");
      }
      if (upload && !upload.transport && metadata.transport === "r2") {
        upload = { ...upload, transport: "r2" };
        await this.storage.put(storageKey, upload);
      }
      if (!upload) {
        const extension = metadata.media_type === "video/quicktime" ? "mov"
          : metadata.media_type === "image/jpeg" ? "jpg" : metadata.media_type.split("/")[1]!;
        const path = `/brain/attachments/${id}/original.${extension}`;
        const key = `brains/${this.sessionId}/${path.slice("/brain/".length)}`;
        const multipart = await this.bucket.createMultipartUpload(key, {
          httpMetadata: { contentType: metadata.media_type },
          customMetadata: { attachment_id: id },
        });
        upload = { ...metadata, path, key, uploadId: multipart.uploadId, count: 0, created: Date.now() };
        // Retain ownership before checking deletion so cleanup can abort it.
        await this.storage.put(storageKey, upload);
        this.#checkActive();
      }
      const object = await this.bucket.head(upload.key);
      if (!object && Date.now() - upload.created > 24 * 60 * 60 * 1000) {
        await this.bucket.resumeMultipartUpload(upload.key, upload.uploadId).abort();
        const multipart = await this.bucket.createMultipartUpload(upload.key, {
          httpMetadata: { contentType: upload.media_type },
          customMetadata: { attachment_id: id },
        });
        if (upload.preview && !await this.bucket.head(upload.preview.key)) {
          await this.bucket.resumeMultipartUpload(upload.preview.key, upload.preview.uploadId).abort();
          delete upload.preview;
        }
        const stale = await this.storage.list({ prefix: storageKey + ":" });
        const keys = [...stale.keys()];
        for (let i = 0; i < keys.length; i += 128) await this.storage.delete(keys.slice(i, i + 128));
        upload = { ...upload, uploadId: multipart.uploadId, count: 0, created: Date.now() };
        await this.storage.put(storageKey, upload);
      }
      this.#checkActive();
      return reply({ id, transport: upload.transport, path: upload.path, size: upload.size, part_size: attachmentPartSize(upload.size),
        next_part: upload.count + 1, complete: object?.size === upload.size });
    }
    if (!upload) throw new AttachmentFailure(404, "attachment_not_found");
    if (upload.transport === "r2" && request.method === "POST" && (action === "preview" || action === "preview/complete")) {
      const key = `brains/${this.sessionId}/attachments/${id}/preview.jpg`;
      if (action === "preview") {
        const intent = parseIntent(await controlJSON(request, this.#readers, () => this.#checkActive()));
        if (intent.size > 2 * 1024 * 1024) throw new AttachmentFailure(400, "invalid_attachment_preview");
        if (upload.preview && !sameIntent(upload.preview, intent)) throw new AttachmentFailure(409, "attachment_part_conflict");
        if (!upload.preview && await this.bucket.head(key)) return reply({ complete: true });
        if (!upload.preview) {
          const multipart = await this.bucket.createMultipartUpload(key, { httpMetadata: { contentType: "image/jpeg" } });
          upload = { ...upload, preview: { ...intent, key, uploadId: multipart.uploadId } };
          await this.storage.put(storageKey, upload);
        }
        this.#checkActive();
        if (upload.preview!.sealed) return reply({ complete: true });
        return reply(await this.#sign(upload.preview!, 1, intent));
      }
      const etag = parseETag(await controlJSON(request, this.#readers, () => this.#checkActive()));
      const preview = upload.preview;
      if (!preview) throw new AttachmentFailure(409, "attachment_incomplete");
      if (etag !== md5Hex(preview.md5)) throw new AttachmentFailure(409, "attachment_part_conflict");
      if (preview.etag && preview.etag !== etag) throw new AttachmentFailure(409, "attachment_part_conflict");
      // Retain the completion ETag before sealing: a lost response must not
      // allow a different acknowledgement to claim the already sealed object.
      upload = { ...upload, preview: { ...preview, etag } };
      await this.storage.put(storageKey, upload);
      let object = await this.bucket.head(key);
      if (!object) object = await this.bucket.resumeMultipartUpload(key, preview.uploadId).complete([{ partNumber: 1, etag }]);
      if (object.size !== preview.size) {
        await this.bucket.delete(key);
        const reset = { ...upload };
        delete reset.preview;
        await this.storage.put(storageKey, reset);
        throw new AttachmentFailure(409, "attachment_size_mismatch");
      }
      await this.storage.put(storageKey, { ...upload, preview: { ...preview, etag, sealed: true } });
      this.#checkActive();
      return reply({ complete: true });
    }
    if (action === "preview") {
      const key = `brains/${this.sessionId}/attachments/${id}/preview.jpg`;
      if (request.method === "PUT") {
        if (upload.transport === "r2") throw new AttachmentFailure(405, "method_not_allowed");
        if (!request.body || request.headers.get("content-type")?.split(";")[0] !== "image/jpeg") {
          throw new AttachmentFailure(400, "invalid_attachment_preview");
        }
        if (!await this.bucket.head(key)) {
          await this.bucket.put(key, request.body, { httpMetadata: { contentType: "image/jpeg" } });
        }
        this.#checkActive();
        return reply({ complete: true });
      }
      if (request.method === "GET") {
        const object = await this.bucket.get(key);
        this.#checkActive();
        if (!object) throw new AttachmentFailure(404, "attachment_preview_not_found");
        return new Response(object.body, { headers: {
          "content-type": "image/jpeg", "content-length": String(object.size), "etag": object.httpEtag,
          "cache-control": "private, max-age=31536000, immutable", "x-content-type-options": "nosniff",
        } });
      }
      throw new AttachmentFailure(405, "method_not_allowed");
    }
    const directMatch = action.match(/^parts\/([1-9][0-9]{0,4})(\/complete)?$/);
    if (upload.transport === "r2" && request.method === "POST" && directMatch) {
      const number = Number(directMatch[1]);
      const partSize = attachmentPartSize(upload.size);
      if (number > Math.ceil(upload.size / partSize) || number > upload.count + 1) throw new AttachmentFailure(409, "attachment_part_order");
      const intentKey = `${storageKey}:intent:${number}`;
      const partKey = `${storageKey}:part:${number}`;
      const retained = await this.storage.get<Intent>(intentKey);
      if (!directMatch[2]) {
        const intent = parseIntent(await controlJSON(request, this.#readers, () => this.#checkActive()));
        if (intent.size !== Math.min(partSize, upload.size - (number - 1) * partSize)) throw new AttachmentFailure(400, "attachment_part_size");
        if (retained && !sameIntent(retained, intent)) throw new AttachmentFailure(409, "attachment_part_conflict");
        if (number <= upload.count) return reply({ complete: true, part: number });
        if (!retained) await this.storage.put(intentKey, intent);
        this.#checkActive();
        return reply(await this.#sign(upload, number, intent));
      }
      const etag = parseETag(await controlJSON(request, this.#readers, () => this.#checkActive()));
      if (!retained) throw new AttachmentFailure(409, "attachment_incomplete");
      if (etag !== md5Hex(retained.md5)) throw new AttachmentFailure(409, "attachment_part_conflict");
      if (number <= upload.count) {
        const part = await this.storage.get<R2UploadedPart>(partKey);
        if (part?.etag !== etag) throw new AttachmentFailure(409, "attachment_part_conflict");
      } else {
        await this.storage.put({ [partKey]: { partNumber: number, etag }, [storageKey]: { ...upload, count: number } });
      }
      this.#checkActive();
      return reply({ part: number });
    }
    const partMatch = action.match(/^parts\/([1-9][0-9]{0,4})$/);
    if (request.method === "PUT" && partMatch) {
      if (upload.transport === "r2") throw new AttachmentFailure(405, "method_not_allowed");
      const number = Number(partMatch[1]);
      const partSize = attachmentPartSize(upload.size);
      const count = Math.ceil(upload.size / partSize);
      if (number > count || number > upload.count + 1) throw new AttachmentFailure(409, "attachment_part_order");
      const expected = Math.min(partSize, upload.size - (number - 1) * partSize);
      const partKey = `${storageKey}:part:${number}`;
      if (number <= upload.count) {
        const retained = await this.storage.get<Part>(partKey);
        // Accepted retries are only hashed; conflicting bytes never overwrite R2.
        const { sha256 } = await this.#partBody(request, expected);
        if (retained?.sha256 !== sha256) throw new AttachmentFailure(409, "attachment_part_conflict");
        return reply({ part: number });
      }
      const { sha256, part } = await this.#partBody(request, expected,
        (body) => this.bucket.resumeMultipartUpload(upload.key, upload.uploadId).uploadPart(number, body));
      this.#checkActive();
      await this.storage.put({ [partKey]: { ...part, sha256 }, [storageKey]: { ...upload, count: number } });
      return reply({ part: number });
    }
    if (request.method === "POST" && action === "complete") {
      let object = await this.bucket.head(upload.key);
      if (!object) {
        if (upload.count !== Math.ceil(upload.size / attachmentPartSize(upload.size))) throw new AttachmentFailure(409, "attachment_incomplete");
        const stored = await this.storage.list<Part>({ prefix: `${storageKey}:part:` });
        const parts = [...stored.values()].filter(({ partNumber }) => partNumber <= upload.count)
          .sort((a, b) => a.partNumber - b.partNumber);
        if (parts.length !== upload.count) throw new AttachmentFailure(409, "attachment_incomplete");
        this.#checkActive();
        object = await this.bucket.resumeMultipartUpload(upload.key, upload.uploadId).complete(parts);
      }
      this.#checkActive();
      if (object.size !== upload.size) {
        await this.bucket.delete(upload.key);
        const multipart = await this.bucket.createMultipartUpload(upload.key, {
          httpMetadata: { contentType: upload.media_type }, customMetadata: { attachment_id: id },
        });
        await this.storage.put(storageKey, { ...upload, uploadId: multipart.uploadId, count: 0, created: Date.now() });
        const stale = [...(await this.storage.list({ prefix: storageKey + ":" })).keys()];
        for (let i = 0; i < stale.length; i += 128) await this.storage.delete(stale.slice(i, i + 128));
        throw new AttachmentFailure(409, "attachment_size_mismatch");
      }
      return reply({ id, path: upload.path, size: upload.size, complete: true });
    }
    if (request.method === "GET" && action === "") {
      const range = request.headers.get("range");
      const object = await this.bucket.get(upload.key, range ? { range: request.headers } : undefined);
      this.#checkActive();
      if (!object || !("body" in object)) throw new AttachmentFailure(404, "attachment_not_found");
      const headers = new Headers({ "cache-control": "private, no-store", "x-content-type-options": "nosniff",
        "content-type": upload.media_type, "accept-ranges": "bytes", "etag": object.httpEtag });
      if (range && object.range && "offset" in object.range && typeof object.range.offset === "number" && "length" in object.range && typeof object.range.length === "number") {
        const { offset, length } = object.range;
        headers.set("content-range", `bytes ${offset}-${offset + length - 1}/${object.size}`);
        headers.set("content-length", String(length));
      } else headers.set("content-length", String(object.size));
      return new Response(object.body, { status: headers.has("content-range") ? 206 : 200, headers });
    }
    throw new AttachmentFailure(405, "method_not_allowed");
  }
}

function parseMetadata(value: unknown): Metadata {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new AttachmentFailure(400, "invalid_attachment");
  const metadata = value as Metadata;
  if (Object.keys(value).sort().join(",") !== (metadata.transport === "r2" ? "media_type,name,size,transport" : "media_type,name,size")
    || typeof metadata.name !== "string" || !metadata.name
    || /[\x00-\x1f\x7f/\\]/.test(metadata.name) || metadata.name === "." || metadata.name === ".."
    || typeof metadata.media_type !== "string"
    || (!["video/mp4", "video/quicktime"].includes(metadata.media_type) && !/^image\/[a-z0-9][a-z0-9.+-]*$/.test(metadata.media_type))
    || !Number.isSafeInteger(metadata.size) || metadata.size < 1 || metadata.size > (metadata.transport === "r2" ? ATTACHMENT_DIRECT_MAX_BYTES : ATTACHMENT_MAX_BYTES)) {
    throw new AttachmentFailure(400, "invalid_attachment");
  }
  return metadata;
}

class AttachmentFailure extends Error { constructor(readonly status: number, code: string) { super(code); } }
function reply(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
}

// Bound control data while streaming; Content-Length is not a trustworthy limit.
async function controlJSON(request: Request, readers: Set<ReadableStreamDefaultReader<Uint8Array>>, checkActive: () => void): Promise<unknown> {
  if (!request.body) throw new AttachmentFailure(400, "invalid_attachment");
  const reader = request.body.getReader();
  readers.add(reader);
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      checkActive();
      if (done) break;
      size += value.length;
      if (size > 16 * 1024) throw new AttachmentFailure(400, "invalid_attachment");
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    try { return JSON.parse(new TextDecoder().decode(bytes)); }
    catch { throw new AttachmentFailure(400, "invalid_attachment"); }
  } finally { await reader.cancel().catch(() => {}); readers.delete(reader); }
}
function parseIntent(value: unknown): Intent {
  const intent = value as Intent;
  if (!intent || typeof intent !== "object" || Object.keys(intent).sort().join(",") !== "md5,size"
    || !Number.isSafeInteger(intent.size) || intent.size < 1 || typeof intent.md5 !== "string"
    || !/^[A-Za-z0-9+/]{22}==$/.test(intent.md5) || btoa(atob(intent.md5)) !== intent.md5) {
    throw new AttachmentFailure(400, "invalid_attachment_part");
  }
  return intent;
}
function sameIntent(a: Intent, b: Intent): boolean { return a.size === b.size && a.md5 === b.md5; }
function parseETag(value: unknown): string {
  const etag = (value as { etag?: unknown } | null)?.etag;
  if (!value || typeof value !== "object" || Object.keys(value).join(",") !== "etag"
    || typeof etag !== "string" || !/^[\x21-\x7e]{1,1024}$/.test(etag)) throw new AttachmentFailure(400, "invalid_attachment_etag");
  const normalized = etag.startsWith('"') && etag.endsWith('"') ? etag.slice(1, -1) : etag;
  if (!normalized) throw new AttachmentFailure(400, "invalid_attachment_etag");
  return normalized;
}

function md5Hex(md5: string): string {
  return Array.from(atob(md5), (byte) => byte.charCodeAt(0).toString(16).padStart(2, "0")).join("");
}
