import { describe, expect, it, vi } from "vitest";

import { persistEventStreamFailure } from "../src/event-stream-failure";

describe("managed event stream failure persistence", () => {
  it("retains the durable failure fence when the replay notice cannot allocate another row", () => {
    const storage = new FailureStorage();
    const appendNotice = vi.fn(() => {
      storage.eventRows += 1;
      throw new Error("SQLITE_FULL: database or disk is full");
    });

    const result = persistEventStreamFailure(
      storage as unknown as DurableObjectStorage,
      "event projection failed: SQLITE_FULL",
      42,
      appendNotice,
    );

    expect(result).toMatchObject({
      noticeError: expect.objectContaining({ message: "SQLITE_FULL: database or disk is full" }),
    });
    expect(storage.streamError).toBe("event projection failed: SQLITE_FULL");
    expect(storage.lastActive).toBe(42);
    expect(storage.eventRows).toBe(0);
    expect(appendNotice).toHaveBeenCalledOnce();
  });

  it("does not attempt the replay notice when the authoritative fence itself cannot persist", () => {
    const storage = new FailureStorage();
    storage.failFence = true;
    const appendNotice = vi.fn(() => ({ cursor: "1" }));

    const result = persistEventStreamFailure(
      storage as unknown as DurableObjectStorage,
      "event projection failed: unavailable",
      43,
      appendNotice,
    );

    expect(result).toMatchObject({
      fenceError: expect.objectContaining({ message: "session fence unavailable" }),
    });
    expect(storage.streamError).toBeNull();
    expect(appendNotice).not.toHaveBeenCalled();
  });
});

class FailureStorage {
  eventRows = 0;
  failFence = false;
  lastActive = 0;
  streamError: string | null = null;

  readonly sql = {
    exec: (_query: string, detail: string, now: number) => {
      if (this.failFence) throw new Error("session fence unavailable");
      this.streamError = detail;
      this.lastActive = now;
      return emptyCursor();
    },
  };

  transactionSync<Result>(callback: () => Result): Result {
    const rows = this.eventRows;
    try {
      return callback();
    } catch (error) {
      this.eventRows = rows;
      throw error;
    }
  }
}

function emptyCursor(): SqlStorageCursor<Record<string, SqlStorageValue>> {
  return {
    columnNames: [],
    one: () => { throw new Error("cursor is empty"); },
    raw: function* () {},
    rowsRead: 0,
    rowsWritten: 0,
    toArray: () => [],
    [Symbol.iterator]: function* () {},
  } as unknown as SqlStorageCursor<Record<string, SqlStorageValue>>;
}
