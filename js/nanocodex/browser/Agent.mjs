import {
  createManagedAgent,
  managedTransportOptions,
} from "../runtime/managed-transport.mjs";

/** Create the browser Agent in its package-owned module Worker. */
export function create(options = {}) {
  if (options.harness === 'xai') return import('./Xai.mjs').then(({ create }) => create(options));
  if (options.harness === 'claude') return import('./Claude.mjs').then(({ create }) => create(options));
  if (options.harness !== undefined && options.harness !== false && options.harness !== 'codex') throw new TypeError('unsupported harness family');
  if (options.harnesses !== undefined) return import('./InlineAgent.mjs').then(({ create }) =>
    create(options.harness === false ? { ...options, harness: 'codex' } : options));
  if (managedTransportOptions(options?.transport)) return createManagedAgent(options);
  return import("./WorkerAgent.mjs").then(({ createWorkerAgent }) =>
    createWorkerAgent(options));
}
