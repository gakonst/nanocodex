import type { NamedTool } from "./types.mjs";

export type DatasetOptions = Readonly<{
  /** Fetch implementation used for dataset metadata, ranges, and streams. */
  fetch?: typeof globalThis.fetch | undefined;
}>;

/** A lazy, session-scoped inspector for public Parquet, JSONL, and Hugging Face datasets. */
export function dataset(options?: DatasetOptions): NamedTool;
