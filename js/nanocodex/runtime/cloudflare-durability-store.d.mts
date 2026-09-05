import type {
  DurabilitySqliteRow,
  DurabilitySqliteValue,
  DurabilityPortableStore,
} from "../types.mjs";

export type CloudflareDurableObjectStorage = Readonly<{
  sql: Readonly<{
    exec<Row extends DurabilitySqliteRow>(
      sql: string,
      ...bindings: readonly DurabilitySqliteValue[]
    ): Readonly<{
      toArray(): readonly Row[];
      [Symbol.iterator](): IterableIterator<Row>;
    }>;
  }>;
  transactionSync<Result>(callback: () => Result): Result;
}>;

/**
 * Initializes and adapts one Durable Object's colocated SQLite durable state.
 * Rust owns the opaque total-state payload; the host only provides atomic storage.
 */
export declare function createCloudflareDurabilityStore(
  storage: CloudflareDurableObjectStorage,
): DurabilityPortableStore;
