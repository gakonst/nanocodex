/** Streams provider bytes while retaining enough overlap to fence reflected credentials. */
export function credentialFilteringBody(
  body: ReadableStream<Uint8Array>,
  credentials: string[],
): ReadableStream<Uint8Array> {
  const patterns = credentials.map((value) => new TextEncoder().encode(value));
  const hold = Math.max(0, ...patterns.map((pattern) => pattern.byteLength - 1));
  const reader = body.getReader();
  let tail = new Uint8Array();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        while (true) {
          const { done, value } = await reader.read();
          if (done) {
            if (containsPattern(tail, patterns)) {
              controller.error(new Error("credential_projection_blocked"));
            } else {
              if (tail.byteLength > 0) controller.enqueue(tail);
              controller.close();
            }
            reader.releaseLock();
            return;
          }
          const combined = concatenate(tail, value);
          if (containsPattern(combined, patterns)) {
            await reader.cancel().catch(() => {});
            reader.releaseLock();
            controller.error(new Error("credential_projection_blocked"));
            return;
          }
          const emitLength = Math.max(0, combined.byteLength - hold);
          tail = combined.slice(emitLength);
          if (emitLength > 0) {
            controller.enqueue(combined.slice(0, emitLength));
            return;
          }
        }
      } catch (error) {
        await reader.cancel(error).catch(() => {});
        reader.releaseLock();
        controller.error(error);
      }
    },
    async cancel(reason) {
      await reader.cancel(reason).catch(() => {});
      reader.releaseLock();
    },
  });
}

function concatenate(left: Uint8Array, right: Uint8Array): Uint8Array {
  const result = new Uint8Array(left.byteLength + right.byteLength);
  result.set(left);
  result.set(right, left.byteLength);
  return result;
}

function containsPattern(value: Uint8Array, patterns: Uint8Array[]): boolean {
  return patterns.some((pattern) => indexOfBytes(value, pattern) !== -1);
}

function indexOfBytes(value: Uint8Array, pattern: Uint8Array): number {
  if (pattern.byteLength === 0 || pattern.byteLength > value.byteLength) return -1;
  outer: for (let index = 0; index <= value.byteLength - pattern.byteLength; index += 1) {
    for (let offset = 0; offset < pattern.byteLength; offset += 1) {
      if (value[index + offset] !== pattern[offset]) continue outer;
    }
    return index;
  }
  return -1;
}
