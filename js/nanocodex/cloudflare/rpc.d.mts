/// <reference lib="esnext.disposable" />

/**
 * Copy a data-only RPC result and dispose its owner, including on clone failure.
 * Returns detached structured data; rejects RPC stubs/functions. Do not use for
 * streams or capabilities that must survive the result (use dup/dispose there).
 * Plain local adapters and primitive/undefined results are also accepted.
 */
export declare function consumeRpcData<T>(value: T): T extends object ? Omit<T, typeof Symbol.dispose> : T;
