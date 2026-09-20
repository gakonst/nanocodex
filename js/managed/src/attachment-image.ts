import { viewImage } from "nanocodex/tools";

export const VIEW_IMAGE_MAX_BYTES = 10 * 1024 * 1024;
type ImageBucket = Pick<R2Bucket, "get">;
type Workspace = { readFile(path: string): Promise<Uint8Array> };

/** Originals stay in R2; only the bounded model image enters the Worker heap.
 * https://developers.cloudflare.com/images/optimization/binding/
 * The service has its own input/format limits; attachment previews cover those.
 */
export function createR2ViewImage(options: {
  bucket: ImageBucket;
  resourceId: string;
  images?: ImagesBinding;
  fallbackWorkspace: Workspace;
  relativePathsUseBrain?: boolean;
}) {
  if (!/^[A-Za-z0-9._:-]{1,256}$/.test(options.resourceId) || options.resourceId === "." || options.resourceId === "..") {
    throw new Error("brain workspace has an invalid resource id");
  }
  return viewImage({ workspace: options.fallbackWorkspace, loadImage: async (path, detail) => {
    if (!path.startsWith("/") && options.relativePathsUseBrain !== false) {
      const parts: string[] = [];
      for (const part of path.split("/")) {
        if (!part || part === ".") continue;
        if (part === "..") {
          if (!parts.length) throw new Error("view_image path must stay beneath /brain");
          parts.pop();
        } else parts.push(part);
      }
      path = `/brain/${parts.join("/")}`;
    }
    if (!path.startsWith("/brain/")) return undefined;
    const relative = path.slice("/brain/".length);
    if (relative.split("/").some((part) => !part || part === "." || part === "..") || /[\\\x00]/.test(relative)) {
      throw new Error("view_image requires a canonical /brain path");
    }
    const key = `brains/${options.resourceId}/${relative}`;
    const object = await options.bucket.get(key);
    if (!object) return undefined;
    if (detail === "original") return { bytes: await boundedImage(object.body, object.size) };
    if (options.images) {
      try {
        const output = await options.images.input(object.body)
          .transform({ width: 2048, height: 2048, fit: "scale-down" })
          .output({ format: "image/jpeg", quality: 90 });
        const response = output.response();
        if (!response.ok || !response.body) throw new Error("image transformation failed");
        return { bytes: await boundedImage(response.body) };
      } catch {
        await object.body.cancel().catch(() => {});
        // Includes unsupported source formats and the Images service input limit.
        const preview = await attachmentPreview(options.bucket, key);
        if (preview) return preview;
        throw new Error("view_image could not transform this original and no attachment preview is available");
      }
    }
    // Small native images remain useful in local/test environments without Images.
    if (object.size <= VIEW_IMAGE_MAX_BYTES) {
      const bytes = await boundedImage(object.body, object.size);
      if (supportedImage(bytes)) return { bytes };
      const preview = await attachmentPreview(options.bucket, key);
      if (preview) return preview;
      throw new Error("view_image requires the IMAGES binding for this source format; no attachment preview is available");
    }
    await object.body.cancel().catch(() => {});
    const preview = await attachmentPreview(options.bucket, key);
    if (preview) return preview;
    throw new Error("view_image requires the IMAGES binding to resize originals over 10 MiB; no attachment preview is available");
  } });
}

async function attachmentPreview(bucket: ImageBucket, key: string) {
  if (!/\/attachments\/[0-9a-f-]{36}\/original\.[^/]+$/.test(key)) return undefined;
  const preview = await bucket.get(key.replace(/original\.[^/]+$/, "preview.jpg"));
  if (!preview) return undefined;
  return { bytes: await boundedImage(preview.body, preview.size),
    note: "Showing the attachment's JPEG preview; the original could not be transformed in this environment." };
}

async function boundedImage(body: ReadableStream<Uint8Array>, size?: number): Promise<Uint8Array> {
  if (size !== undefined && size > VIEW_IMAGE_MAX_BYTES) {
    await body.cancel().catch(() => {});
    throw new Error("view_image original exceeds 10 MiB; use detail high for a resized image");
  }
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > VIEW_IMAGE_MAX_BYTES) throw new Error("view_image output exceeds 10 MiB");
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return bytes;
}

// Same supported source signatures as the public view_image contract.
function supportedImage(bytes: Uint8Array): boolean {
  const starts = (signature: number[], offset = 0) => signature.every((byte, index) => bytes[offset + index] === byte);
  return starts([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
    || starts([0xff, 0xd8, 0xff]) || starts([0x47, 0x49, 0x46, 0x38])
    || (starts([0x52, 0x49, 0x46, 0x46]) && starts([0x57, 0x45, 0x42, 0x50], 8));
}
