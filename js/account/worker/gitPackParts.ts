import type { RepositoryPart } from "./gitRepository.ts";

export async function createRepositoryPartsStream(
  bucket: R2Bucket,
  parts: readonly RepositoryPart[],
): Promise<ReadableStream<Uint8Array>> {
  let partIndex = 0;
  let currentPart: RepositoryPart | undefined;
  let currentBytes = 0;
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        while (partIndex < parts.length || reader != null) {
          if (reader == null) {
            currentPart = parts[partIndex++]!;
            currentBytes = 0;
            const object = await bucket.get(currentPart.key);
            if (object == null) {
              throw new Error(`repository part is missing: ${currentPart.key}`);
            }
            if (object.size !== currentPart.size) {
              throw new Error(`repository part has an invalid size: ${currentPart.key}`);
            }
            reader = object.body.getReader();
          }
          const next = await reader.read();
          if (!next.done) {
            currentBytes += next.value.byteLength;
            if (currentPart == null || currentBytes > currentPart.size) {
              throw new Error(`repository part has an invalid body: ${currentPart?.key ?? "unknown"}`);
            }
            controller.enqueue(next.value);
            return;
          }
          if (currentPart == null || currentBytes !== currentPart.size) {
            throw new Error(`repository part has an invalid body: ${currentPart?.key ?? "unknown"}`);
          }
          reader.releaseLock();
          reader = undefined;
          currentPart = undefined;
        }
        controller.close();
      } catch (error) {
        await reader?.cancel(error).catch(() => undefined);
        reader = undefined;
        controller.error(error);
      }
    },
    async cancel(reason) {
      await reader?.cancel(reason);
      reader = undefined;
    },
  });
}
