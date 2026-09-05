import { describe, expect, it } from "vitest";

import { drainRuntimeForDeletion } from "../src/deletion-runtime";

describe("managed runtime deletion drain", () => {
  it("bounds noncooperative turn cancellation", async () => {
    const turn = {
      cancel: () => new Promise<void>(() => {}),
      dispose: () => {},
    };
    await expect(within(
      drainRuntimeForDeletion(10, [turn] as never[], async () => {}, []),
      "turn cancellation",
    )).rejects.toThrow(/managed runtime deletion drain timed out/);
  });

  it("bounds noncooperative runtime shutdown", async () => {
    await expect(within(
      drainRuntimeForDeletion(10, [], () => new Promise<void>(() => {}), []),
      "runtime shutdown",
    )).rejects.toThrow(/managed runtime deletion drain timed out/);
  });

  it("bounds retained in-flight work", async () => {
    await expect(within(
      drainRuntimeForDeletion(10, [], async () => {}, [new Promise<void>(() => {})]),
      "in-flight work",
    )).rejects.toThrow(/managed runtime deletion drain timed out/);
  });
});

async function within<Result>(promise: Promise<Result>, operation: string): Promise<Result> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`${operation} test timed out`)), 500);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
