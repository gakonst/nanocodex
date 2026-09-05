import { sha1 } from "@noble/hashes/legacy";

import type { GitObjectManifest, GitObjectRecord } from "./gitObjectManifest.ts";

type SelectedRecord = { oid: string; record: GitObjectRecord };

export function createSelectedPackStream(
  bucket: R2Bucket,
  manifest: GitObjectManifest,
  objectIds: readonly string[],
): ReadableStream<Uint8Array> {
  const groups = groupObjectsByShard(manifest, objectIds);
  const hash = sha1.create();
  const header = packHeader(objectIds.length);
  let phase: "header" | "shards" | "trailer" | "done" = "header";
  let groupIndex = 0;

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        if (phase === "header") {
          hash.update(header);
          controller.enqueue(header);
          phase = "shards";
          return;
        }
        if (phase === "shards" && groupIndex < groups.length) {
          const group = groups[groupIndex++]!;
          const shard = manifest.shards[group.shard]!;
          const stored = await bucket.get(shard.key);
          if (stored == null) throw new Error(`Git object shard is missing: ${shard.key}`);
          const contents = new Uint8Array(await stored.arrayBuffer());
          if (contents.byteLength !== shard.size) {
            throw new Error(`Git object shard has an invalid size: ${shard.key}`);
          }
          const selected = concatenate(group.objects.map(({ record }) =>
            contents.subarray(record[2], record[2] + record[3])
          ));
          hash.update(selected);
          controller.enqueue(selected);
          return;
        }
        if (phase === "shards") phase = "trailer";
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

function groupObjectsByShard(
  manifest: GitObjectManifest,
  objectIds: readonly string[],
): Array<{ shard: number; objects: SelectedRecord[] }> {
  const groups = new Map<number, SelectedRecord[]>();
  for (const oid of objectIds) {
    const record = manifest.objects[oid];
    if (record == null) throw new Error(`Git object is absent from the manifest: ${oid}`);
    const objects = groups.get(record[1]) ?? [];
    objects.push({ oid, record });
    groups.set(record[1], objects);
  }
  return [...groups.entries()]
    .sort(([left], [right]) => left - right)
    .map(([shard, objects]) => ({
      shard,
      objects: objects.sort((left, right) => left.record[2] - right.record[2]),
    }));
}

function packHeader(objectCount: number): Uint8Array {
  if (!Number.isSafeInteger(objectCount) || objectCount < 0 || objectCount > 0xffffffff) {
    throw new Error("Git pack object count is out of range");
  }
  const header = new Uint8Array(12);
  header.set(new TextEncoder().encode("PACK"));
  const view = new DataView(header.buffer);
  view.setUint32(4, 2);
  view.setUint32(8, objectCount);
  return header;
}

function concatenate(chunks: readonly Uint8Array[]): Uint8Array {
  const length = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
