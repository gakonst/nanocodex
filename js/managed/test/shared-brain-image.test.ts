import { describe, expect, it, vi } from "vitest";
import { imageGeneration, viewImage } from "nanocodex/tools";
import { createSharedBrainReadWorkspace } from "../src/index";

const maximum = 10 * 1024 * 1024;
const path = "/brain/uploads/original.png";
const fallback = () => ({ readFile: vi.fn(async () => new Uint8Array([9])) });

describe("shared brain image reads", () => {
  it("rejects a huge original from metadata without fetching its body for either image tool", async () => {
    const bucket = {
      head: vi.fn(async () => ({ size: 512 * 1024 * 1024 })),
      get: vi.fn(() => { throw new Error("must not fetch original"); }),
    };
    const other = fallback();
    const workspace = createSharedBrainReadWorkspace(bucket as unknown as R2Bucket, "image-test", other);
    const fetch = vi.fn();
    await expect(viewImage({ workspace }).handler({ path }, {} as never)).rejects.toThrow("10 MiB");
    await expect(imageGeneration({ workspace, fetch }).handler({
      prompt: "edit image", referenced_image_paths: [path],
    }, {} as never)).rejects.toThrow("10 MiB");
    await expect(workspace.readFile("uploads/original.png")).rejects.toThrow("Use the attachment preview_path");
    await expect(workspace.readFile("./uploads/original.png")).rejects.toThrow("10 MiB");
    expect(bucket.head).toHaveBeenCalledWith("brains/image-test/uploads/original.png");
    expect(bucket.get).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
    expect(other.readFile).not.toHaveBeenCalled();
  });

  it("rejects a replacement that grew after HEAD without reading the stream", async () => {
    const cancel = vi.fn(async () => {});
    const getReader = vi.fn(() => { throw new Error("must not read original"); });
    const bucket = {
      head: async () => ({ size: 8 }),
      get: async () => ({ size: maximum + 1, body: { cancel, getReader } }),
    };
    const workspace = createSharedBrainReadWorkspace(bucket as unknown as R2Bucket, "image-test", fallback());
    await expect(workspace.readFile(path)).rejects.toThrow("10 MiB");
    expect(cancel).toHaveBeenCalledOnce();
    expect(getReader).not.toHaveBeenCalled();
  });

  it("bounds streamed bytes when metadata understates size and cancels remaining data", async () => {
    const cancel = vi.fn();
    let pulls = 0;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        pulls += 1;
        controller.enqueue(new Uint8Array(pulls === 1 ? maximum : 1));
      },
      cancel,
    }, { highWaterMark: 0 });
    const bucket = { head: async () => ({ size: 8 }), get: async () => ({ size: 8, body }) };
    const workspace = createSharedBrainReadWorkspace(bucket as unknown as R2Bucket, "image-test", fallback());
    await expect(workspace.readFile(path)).rejects.toThrow("10 MiB");
    expect(pulls).toBe(2);
    expect(cancel).toHaveBeenCalledOnce();
    expect(body.locked).toBe(false);
  });

  it("preserves small inline PNG images and fallback routing", async () => {
    const png = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);
    const bucket = {
      head: vi.fn(async () => ({ size: png.length })),
      get: vi.fn(async () => ({ size: png.length, body: new Response(png).body })),
    };
    const other = fallback();
    const workspace = createSharedBrainReadWorkspace(bucket as unknown as R2Bucket, "image-test", other);
    await expect(viewImage({ workspace }).handler({ path, detail: "original" }, {} as never)).resolves.toMatchObject({
      output: [{ type: "input_image", image_url: "data:image/png;base64,iVBORw0KGgo=", detail: "original" }],
    });
    await expect(workspace.readFile("/hand/private.png")).resolves.toEqual(new Uint8Array([9]));
    expect(other.readFile).toHaveBeenCalledWith("/hand/private.png");
    expect(bucket.head).toHaveBeenCalledOnce();
    expect(bucket.get).toHaveBeenCalledOnce();
    const multiplayer = createSharedBrainReadWorkspace(bucket as unknown as R2Bucket, "image-test", other, {
      relativePathsUseBrain: false,
    });
    await expect(multiplayer.readFile("private.png")).resolves.toEqual(new Uint8Array([9]));
    expect(other.readFile).toHaveBeenCalledWith("private.png");
    expect(bucket.head).toHaveBeenCalledOnce();
  });
});
