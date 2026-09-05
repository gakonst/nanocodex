export type {
  DurabilityAcquiredState,
  DurabilityAcquireRequest,
  DurabilityReplaceRequest,
  DurabilityReplaceResult,
  DurabilityFence,
  DurabilityExportCursor,
  DurabilityExportPageRequest,
  DurabilityPortableStatePage,
  DurabilityPortableStateArchive,
  DurabilityPortableStore,
  DurabilityRevision,
  DurabilitySqliteQuery,
  DurabilitySqliteRow,
  DurabilitySqliteTransaction,
  DurabilitySqliteValue,
  DurabilityStore,
  DurabilityStoredState,
  MemoryDurabilityStore,
  SqliteDurabilityStoreOptions,
} from "../types.mjs";

export declare const sqliteDurabilitySchema: readonly string[];

export declare class DurabilityImportConflictError extends Error {
  override readonly name: "DurabilityImportConflictError";
  readonly expectedRevision: import("../types.mjs").DurabilityRevision | undefined;
  readonly actualRevision: import("../types.mjs").DurabilityRevision | undefined;
  constructor(
    stateId: string,
    expectedRevision?: import("../types.mjs").DurabilityRevision,
    actualRevision?: import("../types.mjs").DurabilityRevision,
  );
}

export declare function durabilityRevision(
  /** Numbers must be nonnegative safe integers; use exact decimal text for larger values. */
  value: string | bigint | number,
): import("../types.mjs").DurabilityRevision;

/** Returns SHA-256 over the UTF-8 JSON tuple `[revision, payload]`. */
export declare function durabilityStateDigest(
  state: import("../types.mjs").DurabilityStoredState,
): Promise<string>;

/**
 * Atomically fences the source owner and exports one coherent JSON-safe state.
 * Do not resume the source provider after beginning a cutover.
 */
export declare function exportDurabilityState(
  store: import("../types.mjs").DurabilityStore,
  stateId: string,
): Promise<import("../types.mjs").DurabilityPortableStateArchive>;

/** Restores an exact archive and its stable state identity into an empty destination. */
export declare function importDurabilityState(
  store: import("../types.mjs").DurabilityPortableStore,
  archive: import("../types.mjs").DurabilityPortableStateArchive,
): Promise<import("../types.mjs").DurabilityStoredState>;

/**
 * Fences the source once and returns a deterministic page of its exact `to` revision.
 * `from` is exclusive, `to` is inclusive, and `cursor` resumes the same payload export.
 * Nonzero ranges must supply `fromDigest` so every page carries the exact lineage proof.
 */
export declare function exportDurabilityStatePage(
  store: import("../types.mjs").DurabilityStore,
  stateId: string,
  request: import("../types.mjs").DurabilityExportPageRequest,
): Promise<import("../types.mjs").DurabilityPortableStatePage>;

/** Imports a complete contiguous page set iff the destination still has the exact state at `from`. */
export declare function importDurabilityStatePages(
  store: import("../types.mjs").DurabilityPortableStore,
  pages: Iterable<import("../types.mjs").DurabilityPortableStatePage>,
): Promise<import("../types.mjs").DurabilityStoredState>;

export declare function createMemoryDurabilityStore(
  stateId: string,
  initial?: import("../types.mjs").DurabilityStoredState,
): import("../types.mjs").MemoryDurabilityStore;

export declare function createSqliteDurabilityStore(
  options: import("../types.mjs").SqliteDurabilityStoreOptions,
): import("../types.mjs").DurabilityPortableStore;
