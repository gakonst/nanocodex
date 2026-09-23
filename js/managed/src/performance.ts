import { AsyncLocalStorage } from "node:async_hooks";

type Context = { trace_id: string; scope: string; closed?: boolean; reads: Record<string, { count: number; duration_ms: number }> };
const contexts = new AsyncLocalStorage<Context>();

export function performanceSyncScope<T>(traceId: string, scope: string, run: () => T): T {
  const context: Context = { trace_id: traceId, scope, reads: {} };
  return contexts.run(context, () => {
    const began = performance.now();
    try { return run(); }
    finally { context.closed = true; console.info({ type: "managed.performance", trace_id: traceId, stage: scope,
      duration_ms: performance.now() - began, reads: context.reads }); }
  });
}

/** Timings only: never record query arguments, tool results or credentials. */
export async function performanceScope<T>(traceId: string, scope: string, run: () => Promise<T>): Promise<T> {
  const context: Context = { trace_id: traceId, scope, reads: {} };
  return contexts.run(context, async () => {
    const began = performance.now();
    try { return await run(); }
    finally {
      context.closed = true;
      console.info({ type: "managed.performance", trace_id: traceId, stage: scope,
        duration_ms: performance.now() - began, reads: context.reads });
    }
  });
}

export async function performanceStage<T>(stage: string, run: () => Promise<T>): Promise<T> {
  const context = contexts.getStore();
  if (!context) return run();
  const began = performance.now();
  let success = false;
  try { const result = await run(); success = true; return result; }
  finally { console.info({ type: "managed.performance", trace_id: context.trace_id,
    stage, started_at: Date.now() - (performance.now() - began), duration_ms: performance.now() - began, success }); }
}

/** One bounded discovery record; no owner, authority key or metadata payload. */
export function performanceCache(stage: string, cacheState: "hit" | "miss", ageMs: number, remainingMs: number): void {
  const context = contexts.getStore();
  if (!context) return;
  try {
    console.info({ type: "managed.performance", trace_id: context.trace_id, stage,
      cache_state: cacheState, cache_age_ms: Math.max(0, ageMs), remaining_ttl_ms: Math.max(0, remainingMs) });
  } catch { /* Passive cache observations cannot fail admission. */ }
}

export function performanceRead<T>(table: string, run: () => T): T {
  const context = contexts.getStore();
  if (!context) return run();
  const began = performance.now();
  try { return run(); }
  finally {
    const read = context.reads[table] ??= { count: 0, duration_ms: 0 };
    read.count++;
    read.duration_ms += performance.now() - began;
  }
}

/** Opt-in SQL audit. Preserve native receivers and cursors, including transactions. */
export function performanceState<Props>(state: DurableObjectState<Props>): DurableObjectState<Props> {
  type Statement = { statement_id: string; operation?: string; tables: string[]; exec_ms: number; consume_ms: number; rows_read: number; rows_written: number; success: boolean; count: number };
  const pending: Array<{ trace_id?: string; stage: string; read: () => Statement }> = [];
  const flush = () => {
    const groups = new Map<string, { trace_id?: string; stage: string; statements: Map<string, Statement> }>();
    for (const item of pending.splice(0)) {
      const key = `${item.trace_id ?? ""}\n${item.stage}`;
      let group = groups.get(key);
      if (!group) { group = { trace_id: item.trace_id, stage: item.stage, statements: new Map() }; groups.set(key, group); }
      const statement = item.read();
      const current = group.statements.get(statement.statement_id);
      if (!current) group.statements.set(statement.statement_id, statement);
      else {
        current.count += statement.count;
        current.exec_ms += statement.exec_ms;
        current.consume_ms += statement.consume_ms;
        current.rows_read += statement.rows_read;
        current.rows_written += statement.rows_written;
        current.success &&= statement.success;
      }
    }
    for (const group of groups.values()) console.info({ type: "managed.sql_batch", object_id: state.id.toString(),
      trace_id: group.trace_id, stage: group.stage, statements: [...group.statements.values()] });
  };
  const sql = new Proxy(state.storage.sql, {
    get(target, property) {
      if (property !== "exec") {
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      }
      return (query: string, ...bindings: unknown[]) => {
        const began = performance.now();
        const inherited = contexts.getStore();
        const context = inherited?.closed ? undefined : inherited;
        const normalized = query.replace(/'(?:''|[^'])*'/g, "?").replace(/\b\d+\b/g, "?").replace(/\s+/g, " ").trim();
        let hash = 2166136261;
        for (let i = 0; i < normalized.length; i++) hash = Math.imul(hash ^ normalized.charCodeAt(i), 16777619);
        const tables = [...new Set([...normalized.matchAll(/\b(?:FROM|JOIN|INTO|UPDATE|TABLE(?: IF NOT EXISTS)?)\s+([a-z_][a-z_0-9]*)/gi)].map(match => match[1]!.toLowerCase()))].filter(table => table !== "set");
        let cursor: SqlStorageCursor<Record<string, SqlStorageValue>> | undefined;
        let execMs = 0;
        let consumeMs = 0;
        try {
          cursor = target.exec(query, ...bindings);
          execMs = performance.now() - began;
          const result = cursor;
          const proxy: typeof result = new Proxy(result, {
            get(target, property) {
              if (property === Symbol.iterator) return () => proxy;
              const value = Reflect.get(target, property, target);
              if (typeof value !== "function") return value;
              return (...args: unknown[]) => {
                const started = performance.now();
                try { return value.apply(target, args); }
                finally { consumeMs += performance.now() - started; }
              };
            },
          });
          return proxy;
        }
        finally {
          execMs = performance.now() - began;
          // SQL consumers are synchronous. Snapshot rows after toArray/one/iteration
          // in the current stack, without draining or replacing the native cursor.
          if (pending.length === 0) queueMicrotask(flush);
          pending.push({ trace_id: context?.trace_id, stage: context?.scope ?? "object.background", read: () => ({
            statement_id: (hash >>> 0).toString(16), operation: normalized.match(/^[A-Za-z]+/)?.[0]?.toUpperCase(),
            tables, exec_ms: execMs, consume_ms: consumeMs, rows_read: cursor?.rowsRead ?? 0,
            rows_written: cursor?.rowsWritten ?? 0, success: cursor !== undefined, count: 1 }) });
        }
      };
    },
  });
  const storage = new Proxy(state.storage, {
    get(target, property) {
      if (property === "sql") return sql;
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  return new Proxy(state, {
    get(target, property) {
      if (property === "storage") return storage;
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}
