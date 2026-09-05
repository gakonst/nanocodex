import type { CodeEvaluator } from "../types.mjs";

export type QuickJsEvaluatorOptions = {
  memoryLimitBytes?: number | undefined;
  stackLimitBytes?: number | undefined;
  maxInterruptCycles?: number | undefined;
};

export type AsyncQuickJsModule = {
  newContext(): unknown;
};

/** Builds a serialized, sandboxed Code Mode evaluator for CSP-restricted runtimes. */
export function createQuickJsEvaluator(
  quickJs: AsyncQuickJsModule,
  options?: QuickJsEvaluatorOptions,
): CodeEvaluator;
