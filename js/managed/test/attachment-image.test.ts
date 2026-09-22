import { describe, expect, it, vi } from "vitest";
import { createR2ViewImage, VIEW_IMAGE_MAX_BYTES } from "../src/attachment-image";

const path = "/brain/attachments/11111111-1111-1111-1111-111111111111/original.heic";
const jpeg = new Uint8Array([255, 216, 255, 224, 0]);
function fixture(size = 100, images?: ImagesBinding, preview = false) {
  const body = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(jpeg); c.close(); } });
  const arrayBuffer = vi.fn(() => { throw new Error("original must never be buffered"); });
  const get = vi.fn(async (key: string) => key.endsWith("preview.jpg")
    ? preview ? { body: new Response(jpeg).body!, size: jpeg.length } : null
    : { body, size, arrayBuffer });
  const readFile = vi.fn(async () => jpeg);
  const tool = createR2ViewImage({ bucket: { get } as unknown as R2Bucket, resourceId: "test", images,
    fallbackWorkspace: { readFile } });
  return { tool, body, get, readFile, arrayBuffer };
}
function binding(output: () => Promise<{ response(): Response }>) {
  const transform = vi.fn(() => ({ output }));
  const input = vi.fn(() => ({ transform }));
  return { images: { input } as unknown as ImagesBinding, input, transform };
}
const call = (tool: ReturnType<typeof createR2ViewImage>, args: object) => tool.handler(args, {} as never);

describe("R2 image adapter", () => {
  it("passes the huge original stream directly to Images and returns bounded JPEG", async () => {
    const output = vi.fn(async () => ({ response: () => new Response(jpeg) }));
    const b = binding(output);
    const f = fixture(500 * 1024 * 1024, b.images);
    const result = await call(f.tool, { path });
    expect(b.input).toHaveBeenCalledWith(f.body);
    expect(b.transform).toHaveBeenCalledWith({ width: 2048, height: 2048, fit: "scale-down" });
    expect(output).toHaveBeenCalledWith({ format: "image/jpeg", quality: 90 });
    expect(JSON.stringify(result)).toContain("data:image/jpeg;base64,");
    expect(f.arrayBuffer).not.toHaveBeenCalled();
    expect(f.readFile).not.toHaveBeenCalled();
  });
  it("keeps original exact and never transforms or substitutes a preview", async () => {
    const b = binding(async () => { throw new Error("unused"); });
    const small = fixture(jpeg.length, b.images);
    expect(JSON.stringify(await call(small.tool, { path, detail: "original" }))).toContain("/9j/4AA=");
    const large = fixture(VIEW_IMAGE_MAX_BYTES + 1, b.images, true);
    await expect(call(large.tool, { path, detail: "original" })).rejects.toThrow("original exceeds");
    expect(b.input).not.toHaveBeenCalled();
    expect(large.get).toHaveBeenCalledTimes(1);
  });
  it("uses labeled preview on transformation failure only for high", async () => {
    const b = binding(async () => { throw new Error("unsupported HEIC"); });
    const f = fixture(100, b.images, true);
    expect(JSON.stringify(await call(f.tool, { path }))).toContain("JPEG preview");
  });
  it("reports missing binding for large originals without a preview", async () => {
    const f = fixture(VIEW_IMAGE_MAX_BYTES + 1);
    await expect(call(f.tool, { path })).rejects.toThrow("IMAGES binding");
    expect(f.arrayBuffer).not.toHaveBeenCalled();
  });
  it("bounds transformed bytes even without a content-length", async () => {
    const b = binding(async () => ({ response: () => new Response(new Uint8Array(VIEW_IMAGE_MAX_BYTES + 1)) }));
    await expect(call(fixture(100, b.images).tool, { path })).rejects.toThrow("could not transform");
  });
  it("rejects unsupported exact originals even when a preview exists", async () => {
    const get = vi.fn(async () => ({ size: 4, body: new Response("HEIC").body! }));
    const tool = createR2ViewImage({ bucket: { get } as unknown as R2Bucket, resourceId: "test",
      fallbackWorkspace: { readFile: async () => jpeg } });
    await expect(call(tool, { path, detail: "original" })).rejects.toThrow("supports PNG, JPEG, GIF, and WebP");
    expect(get).toHaveBeenCalledTimes(1);
  });
  it("uses a labeled bounded preview when Images is absent for a large original", async () => {
    const f = fixture(VIEW_IMAGE_MAX_BYTES + 1, undefined, true);
    expect(JSON.stringify(await call(f.tool, { path }))).toContain("JPEG preview");
  });
  it("preserves local workspace fallback", async () => {
    const f = fixture();
    await call(f.tool, { path: "/local/image.jpg" });
    expect(f.readFile).toHaveBeenCalledWith("/local/image.jpg");
    expect(f.get).not.toHaveBeenCalled();
  });
});

it("validates resource scope and handles relative brain paths", async () => {
  expect(() => createR2ViewImage({ bucket: {} as R2Bucket, resourceId: "../other", fallbackWorkspace: { readFile: async () => jpeg } })).toThrow("resource id");
  const f = fixture();
  await call(f.tool, { path: "image.jpg" });
  expect(f.get).toHaveBeenCalledWith("brains/test/image.jpg");
  await expect(call(fixture().tool, { path: "../other/image.jpg" })).rejects.toThrow("beneath /brain");
});
it("uses preview for small unsupported originals when Images is absent", async () => {
  const get = vi.fn(async (key: string) => key.endsWith("preview.jpg")
    ? { size: jpeg.length, body: new Response(jpeg).body! }
    : { size: 4, body: new Response("HEIC").body! });
  const tool = createR2ViewImage({ bucket: { get } as unknown as R2Bucket, resourceId: "test", fallbackWorkspace: { readFile: async () => jpeg } });
  expect(JSON.stringify(await call(tool, { path }))).toContain("JPEG preview");
});
it("keeps relative paths in the fallback workspace when requested", async () => {
  const get = vi.fn();
  const readFile = vi.fn(async () => jpeg);
  const tool = createR2ViewImage({ bucket: { get } as unknown as R2Bucket, resourceId: "test", relativePathsUseBrain: false, fallbackWorkspace: { readFile } });
  await call(tool, { path: "image.jpg" });
  expect(readFile).toHaveBeenCalledWith("image.jpg");
  expect(get).not.toHaveBeenCalled();
});
