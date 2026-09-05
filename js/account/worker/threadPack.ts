import { sha1 } from "@noble/hashes/legacy";

import type { ThreadPack } from "./threadRepository.ts";

const PACK_HEADER_BYTES = 12;
const PACK_TRAILER_BYTES = 20;
const PACK_HEAD_CONCURRENCY = 4;

export async function createThreadPackStream(
  bucket: R2Bucket,
  packs: readonly ThreadPack[],
): Promise<ReadableStream<Uint8Array>> {
  const objectCount = packs.reduce((total, pack) => total + pack.objectCount, 0);
  if (!Number.isSafeInteger(objectCount) || objectCount > 0xffff_ffff) {
    throw new Error("thread repository pack object count exceeds version 2 limits");
  }
  for (let offset = 0; offset < packs.length; offset += PACK_HEAD_CONCURRENCY) {
    const batch = packs.slice(offset, offset + PACK_HEAD_CONCURRENCY);
    const objects = await Promise.all(batch.map((pack) => bucket.head(pack.key)));
    for (let index = 0; index < batch.length; index++) {
      const pack = batch[index]!;
      const object = objects[index];
      if (!object) throw new Error(`thread pack is missing: ${pack.key}`);
      if (object.size !== pack.size) {
        throw new Error(`thread pack has an invalid size: ${pack.key}`);
      }
    }
  }

  const header = packHeader(objectCount);
  const hash = sha1.create();
  let phase: "header" | "packs" | "trailer" | "done" = "header";
  let packIndex = 0;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        if (phase === "header") {
          hash.update(header);
          controller.enqueue(header);
          phase = "packs";
          return;
        }
        if (phase === "packs" && packIndex < packs.length) {
          const pack = packs[packIndex++]!;
          const object = await bucket.get(pack.key);
          if (!object) throw new Error(`thread pack is missing: ${pack.key}`);
          const bytes = new Uint8Array(await object.arrayBuffer());
          validateSourcePack(bytes, pack);
          const entries = bytes.subarray(PACK_HEADER_BYTES, bytes.byteLength - PACK_TRAILER_BYTES);
          hash.update(entries);
          controller.enqueue(entries);
          return;
        }
        if (phase === "packs") phase = "trailer";
        if (phase === "trailer") {
          controller.enqueue(hash.digest());
          phase = "done";
          controller.close();
        }
      } catch (error) {
        phase = "done";
        controller.error(error);
      }
    },
  });
}

function validateSourcePack(bytes: Uint8Array, pack: ThreadPack): void {
  if (bytes.byteLength !== pack.size || bytes.byteLength < PACK_HEADER_BYTES + PACK_TRAILER_BYTES) {
    throw new Error(`thread pack has an invalid size: ${pack.key}`);
  }
  if (new TextDecoder().decode(bytes.subarray(0, 4)) !== "PACK") {
    throw new Error(`thread pack has an invalid header: ${pack.key}`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(4) !== 2 || view.getUint32(8) !== pack.objectCount) {
    throw new Error(`thread pack has invalid metadata: ${pack.key}`);
  }
  const trailer = bytes.subarray(bytes.byteLength - PACK_TRAILER_BYTES);
  const actual = sha1(bytes.subarray(0, bytes.byteLength - PACK_TRAILER_BYTES));
  if (!equalBytes(actual, trailer) || bytesToHex(trailer) !== pack.hash) {
    throw new Error(`thread pack checksum is invalid: ${pack.key}`);
  }
}

function packHeader(objectCount: number): Uint8Array {
  const header = new Uint8Array(PACK_HEADER_BYTES);
  header.set(new TextEncoder().encode("PACK"));
  const view = new DataView(header.buffer);
  view.setUint32(4, 2);
  view.setUint32(8, objectCount);
  return header;
}

function equalBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  let difference = 0;
  for (let index = 0; index < left.byteLength; index++) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

function bytesToHex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
