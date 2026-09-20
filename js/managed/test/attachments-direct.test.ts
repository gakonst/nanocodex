import { createHash } from "node:crypto";
import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { SessionAttachments, ATTACHMENT_DIRECT_MAX_BYTES } from "../src/attachments";
import { createBrainBucket } from "../src/brain-bucket";
import { createBrainWorkspace } from "../src/brain-workspace";
import type { AttachmentUploadSigner } from "../src/attachment-r2";

const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const backing = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
const md5 = createHash("md5").update(new Uint8Array([1, 2, 3, 4])).digest("base64");
const previewMD5 = createHash("md5").update(new Uint8Array([255, 216, 255, 217])).digest("base64");
const metadata = (size = 4) => ({ name: "Original.mp4", media_type: "video/mp4", size, transport: "r2" });
const request = (body?: unknown, method = "POST") => new Request("https://session.internal/attachment", { method, body: body === undefined ? undefined : JSON.stringify(body) });

it("keeps media outside control requests and catalogs direct originals and sealed previews", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    const direct = s3MultipartEtags(backing);
    const bucket = createBrainBucket(ctx.storage, direct, agent);
    const signed: Parameters<AttachmentUploadSigner["signPart"]>[0][] = [];
    const signer: AttachmentUploadSigner = { async signPart(input) { signed.push(input); return { url: "https://r2.example/part", headers: { "content-md5": input.md5 }, expires_at: Date.now() + 60_000 }; } };
    let store = new SessionAttachments(ctx.storage, bucket, agent, () => true, signer);
    const call = (action: string, body?: unknown) => store.fetch(request(body), id, action);
    expect(await (await call("", metadata())).json()).toMatchObject({ transport: "r2", next_part: 1 });
    expect((await store.fetch(request("bytes", "PUT"), id, "parts/1")).status).toBe(405);
    expect((await call("parts/1/complete", { etag: "missing" })).status).toBe(409);
    expect((await call("parts/2", { size: 4, md5 })).status).toBe(409);
    expect((await call("parts/1", { size: 3, md5 })).status).toBe(400);
    expect((await call("parts/1", { size: 4, md5: "bad" })).status).toBe(400);
    expect((await call("parts/1", { size: 4, md5 })).status).toBe(200);
    expect((await call("parts/1", { size: 4, md5: "AQAAAAAAAAAAAAAAAAAAAA==" })).status).toBe(409);
    const original = new Uint8Array([1, 2, 3, 4]);
    const originalIntent = signed.at(-1)!;
    const part = await direct.resumeMultipartUpload(originalIntent.key, originalIntent.uploadId).uploadPart(1, original);
    expect((await call("parts/1/complete", { etag: `"${part.etag}"` })).status).toBe(200);
    store = new SessionAttachments(ctx.storage, bucket, agent, () => true, signer);
    expect((await call("parts/1/complete", { etag: part.etag })).status).toBe(200);
    expect((await call("parts/1/complete", { etag: "different" })).status).toBe(409);
    expect(await (await call("parts/1", { size: 4, md5 })).json()).toEqual({ complete: true, part: 1 });
    expect((await call("complete")).status).toBe(200);
    expect((await call("preview", { size: 2 * 1024 * 1024 + 1, md5 })).status).toBe(400);
    expect((await call("preview", { size: 4, md5: previewMD5 })).status).toBe(200);
    const previewIntent = signed.at(-1)!;
    const preview = new Uint8Array([255, 216, 255, 217]);
    const previewPart = await direct.resumeMultipartUpload(previewIntent.key, previewIntent.uploadId).uploadPart(1, preview);
    expect((await call("preview/complete", { etag: previewPart.etag })).status).toBe(200);
    expect((await call("preview/complete", { etag: previewPart.etag })).status).toBe(200);
    expect((await call("preview/complete", { etag: "different" })).status).toBe(409);
    expect((await call("preview", { size: 4, md5: "AQAAAAAAAAAAAAAAAAAAAA==" })).status).toBe(409);
    const signedCount = signed.length;
    expect(await (await call("preview", { size: 4, md5: previewMD5 })).json()).toEqual({ complete: true });
    expect(signed.length).toBe(signedCount);
    expect((await store.fetch(request("bytes", "PUT"), id, "preview")).status).toBe(405);
    const workspace = createBrainWorkspace(bucket, agent);
    expect(new Uint8Array(await workspace.readFile(`/brain/attachments/${id}/original.mp4`))).toEqual(original);
    expect(new Uint8Array(await workspace.readFile(`/brain/attachments/${id}/preview.jpg`))).toEqual(preview);
    await expect(backing.resumeMultipartUpload(previewIntent.key, previewIntent.uploadId).uploadPart(1, new Uint8Array(4))).rejects.toThrow();
    await store.cleanup();
    expect((await call("preview", { size: 4, md5: previewMD5 })).status).toBe(409);
    await bucket.delete([originalIntent.key, previewIntent.key]);
  });
});

it("bounds metadata, gates signing, permits the R2 maximum, and clears expired intents and unfinished previews", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    const absent = new SessionAttachments(ctx.storage, backing, agent, () => true);
    expect((await absent.fetch(request(metadata()), id)).status).toBe(503);
    const signed: Parameters<AttachmentUploadSigner["signPart"]>[0][] = [];
    const signer: AttachmentUploadSigner = { async signPart(input) { signed.push(input); return { url: "https://r2.example/part", headers: {}, expires_at: Date.now() + 60_000 }; } };
    const store = new SessionAttachments(ctx.storage, backing, agent, () => true, signer);
    const call = (action: string, body: unknown, attachment = id) => store.fetch(request(body), attachment, action);
    expect((await call("", { ...metadata(), name: "x".repeat(20_000) })).status).toBe(400);
    expect((await call("", metadata(ATTACHMENT_DIRECT_MAX_BYTES + 1))).status).toBe(400);
    expect(await (await call("", metadata(ATTACHMENT_DIRECT_MAX_BYTES), crypto.randomUUID())).json()).toMatchObject({ part_size: Math.ceil(ATTACHMENT_DIRECT_MAX_BYTES / 10_000) });
    await call("", metadata());
    await call("parts/1", { size: 4, md5 });
    await call("preview", { size: 4, md5: previewMD5 });
    const old = [...signed];
    const upload = await ctx.storage.get<Record<string, unknown>>("attachment:" + id);
    await ctx.storage.put("attachment:" + id, { ...upload, created: 0 });
    expect(await (await call("", metadata())).json()).toMatchObject({ next_part: 1 });
    expect(await ctx.storage.get("attachment:" + id + ":intent:1")).toBeUndefined();
    expect((await call("preview/complete", { etag: "old" })).status).toBe(409);
    for (const intent of old) await expect(backing.resumeMultipartUpload(intent.key, intent.uploadId).uploadPart(1, new Uint8Array(4))).rejects.toThrow();
    expect((await call("parts/1", { size: 4, md5: "AQAAAAAAAAAAAAAAAAAAAA==" })).status).toBe(200);
    await call("preview", { size: 4, md5: previewMD5 });
    const pending = signed.at(-1)!;
    await store.cleanup();
    await expect(backing.resumeMultipartUpload(pending.key, pending.uploadId).uploadPart(1, new Uint8Array(4))).rejects.toThrow();
    expect((await ctx.storage.list({ prefix: "attachment:" })).size).toBe(0);
  });
});

it("migrates retained legacy parts without issuing overwrite capabilities and preserves a legacy preview", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    const signer: AttachmentUploadSigner = { async signPart() { throw new Error("acknowledged bytes must not be signed again"); } };
    const store = new SessionAttachments(ctx.storage, backing, agent, () => true, signer);
    const { transport: _transport, ...legacy } = metadata();
    await store.fetch(request(legacy), id);
    const bytes = new Uint8Array([1, 2, 3, 4]);
    expect((await store.fetch(new Request("https://internal", { method: "PUT", body: bytes }), id, "parts/1")).status).toBe(200);
    expect((await store.fetch(new Request("https://internal", { method: "PUT", body: bytes, headers: { "content-type": "image/jpeg" } }), id, "preview")).status).toBe(200);
    expect(await (await store.fetch(request(metadata()), id)).json()).toMatchObject({ transport: "r2", next_part: 2 });
    expect(await (await store.fetch(request({ size: 4, md5 }), id, "parts/1")).json()).toEqual({ complete: true, part: 1 });
    expect(await (await store.fetch(request({ size: 4, md5 }), id, "preview")).json()).toEqual({ complete: true });
    expect((await store.fetch(request(), id, "complete")).status).toBe(200);
    expect(new Uint8Array(await (await store.fetch(request(undefined, "GET"), id, "preview")).arrayBuffer())).toEqual(bytes);
    await store.cleanup();
    await backing.delete([`brains/${agent}/attachments/${id}/original.mp4`, `brains/${agent}/attachments/${id}/preview.jpg`]);
  });
});

// Miniflare uses opaque multipart ETags. Model R2's documented MD5 S3
// contract, retaining its real multipart storage and completion validation.
function s3MultipartEtags(bucket: R2Bucket): R2Bucket {
  const parts = new Map<string, R2UploadedPart>();
  return new Proxy(bucket, { get(target, method) {
    if (method === "resumeMultipartUpload") return (key: string, uploadId: string) => {
      const upload = target.resumeMultipartUpload(key, uploadId);
      return new Proxy(upload, { get(targetUpload, operation) {
        if (operation === "uploadPart") return async (number: number, bytes: Uint8Array) => {
          const part = await targetUpload.uploadPart(number, bytes);
          const etag = createHash("md5").update(bytes).digest("hex");
          parts.set(`${uploadId}:${number}:${etag}`, part);
          return { partNumber: number, etag };
        };
        if (operation === "complete") return (selected: R2UploadedPart[]) => targetUpload.complete(selected.map(part =>
          parts.get(`${uploadId}:${part.partNumber}:${part.etag}`) ?? part));
        const value = Reflect.get(targetUpload, operation, targetUpload);
        return typeof value === "function" ? value.bind(targetUpload) : value;
      } });
    };
    const value = Reflect.get(target, method, target);
    return typeof value === "function" ? value.bind(target) : value;
  } });
}

it("binds acknowledgements to MD5 and removes mismatched completed objects from the brain catalog before retry", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    const direct = s3MultipartEtags(backing);
    const brain = createBrainBucket(ctx.storage, direct, agent);
    let mismatch = true;
    const bucket = new Proxy(brain, { get(target, method) {
      if (method === "resumeMultipartUpload") return (key: string, uploadId: string) => {
        const upload = target.resumeMultipartUpload(key, uploadId);
        return new Proxy(upload, { get(targetUpload, operation) {
          if (operation === "complete") return async (parts: R2UploadedPart[]) => {
            const object = await targetUpload.complete(parts);
            return mismatch ? { ...object, size: object.size + 1 } : object;
          };
          const value = Reflect.get(targetUpload, operation, targetUpload);
          return typeof value === "function" ? value.bind(targetUpload) : value;
        } });
      };
      const value = Reflect.get(target, method, target);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const signed: Parameters<AttachmentUploadSigner["signPart"]>[0][] = [];
    const store = new SessionAttachments(ctx.storage, bucket, agent, () => true, { async signPart(input) {
      signed.push(input); return { url: "https://r2.example/part", headers: {}, expires_at: Date.now() + 60_000 };
    } });
    const call = (action: string, body?: unknown) => store.fetch(request(body), id, action);
    await call("", metadata());
    const bytes = new Uint8Array([1, 2, 3, 4]);
    for (const action of ["parts/1", "preview"]) {
      mismatch = true;
      await call(action, { size: 4, md5 });
      const intent = signed.at(-1)!;
      const part = await direct.resumeMultipartUpload(intent.key, intent.uploadId).uploadPart(1, bytes);
      expect((await call(action + "/complete", { etag: "0".repeat(32) })).status).toBe(409);
      const ack = await call(action + "/complete", { etag: `"${part.etag}"` });
      expect(ack.status).toBe(action === "preview" ? 409 : 200);
      if (action !== "preview") expect((await call("complete")).status).toBe(409);
      expect(await bucket.head(intent.key)).toBeNull();
      expect(await backing.head(intent.key)).toBeNull();
      await expect(createBrainWorkspace(bucket, agent).readFile(`/brain/attachments/${id}/${action === "preview" ? "preview.jpg" : "original.mp4"}`)).rejects.toMatchObject({ code: "ENOENT" });
      mismatch = false;
      await call(action, { size: 4, md5 });
      const retry = signed.at(-1)!;
      expect(retry.uploadId).not.toBe(intent.uploadId);
      const retriedPart = await direct.resumeMultipartUpload(retry.key, retry.uploadId).uploadPart(1, bytes);
      expect((await call(action + "/complete", { etag: retriedPart.etag })).status).toBe(200);
      if (action !== "preview") expect((await call("complete")).status).toBe(200);
      expect((await bucket.head(retry.key))?.size).toBe(4);
    }
    await store.cleanup();
    await bucket.delete([`brains/${agent}/attachments/${id}/original.mp4`, `brains/${agent}/attachments/${id}/preview.jpg`]);
  });
});
