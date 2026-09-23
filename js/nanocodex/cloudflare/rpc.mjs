/**
 * Consume a data-only RPC result before returning or caching its detached data.
 * Each receiving hop owns its result, even when the payload has no stubs.
 * structuredClone rejects RPC capabilities; callers retaining capabilities must
 * instead manage their lifetime explicitly (dup/dispose).
 */
export function consumeRpcData(value) {
  try {
    return structuredClone(value);
  } finally {
    // Plain local adapters and primitive/undefined RPC results have no disposer.
    value?.[Symbol.dispose]?.();
  }
}
