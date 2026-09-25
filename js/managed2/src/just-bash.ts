import type { NamedTool, ToolContext } from "nanocodex";
import { justBash, type Workspace } from "nanocodex-tools";

export type BashPhase = "setup" | "vfs_hydrate" | "execute" | "vfs_flush";
export type BashPhaseObserver = (context: ToolContext, phase: BashPhase, durationMs: number) => void;

type Entry = { kind: "directory"; data: null } | { kind: "file"; data: Uint8Array };
type Metadata = { kind: Entry["kind"]; size: number };
const root = "/brain";
const encoder = new TextEncoder();

/** The DO's SQLite is the only durable authority; writes are staged per command. */
class SqliteWorkspace implements Workspace {
  readonly root = root;
  #stage = new Map<string, Entry | null>();
  constructor(private readonly storage: DurableObjectStorage, private readonly onHydrate: (durationMs: number) => void) {
    // This class itself is only constructed on the first exec_command call.
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS bash_files (
      path TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('file', 'directory')),
      data BLOB
    )`);
  }
  private path(input: string): string {
    if (typeof input !== "string" || input.includes("\\") || input.includes("\0")) throw new Error("invalid shell path");
    const parts = (input.startsWith("/") ? input : `${root}/${input}`).split("/");
    const normalized: string[] = [];
    for (const part of parts) {
      if (!part || part === ".") continue;
      if (part === "..") normalized.pop();
      else normalized.push(part);
    }
    const path = `/${normalized.join("/")}`;
    if (path !== root && !path.startsWith(`${root}/`)) throw new Error("shell path escapes /brain");
    return path;
  }
  private metadata(): Map<string, Metadata> {
    const entries = new Map<string, Metadata>([[root, { kind: "directory", size: 0 }]]);
    // Never load file bodies during hydration or a metadata operation.
    for (const row of this.storage.sql.exec<{ path: string; kind: string; size: number }>(
      "SELECT path, kind, length(data) AS size FROM bash_files ORDER BY path",
    )) entries.set(row.path, { kind: row.kind as Metadata["kind"], size: row.size ?? 0 });
    for (const [path, entry] of this.#stage) {
      if (entry === null) entries.delete(path);
      else entries.set(path, { kind: entry.kind, size: entry.data?.byteLength ?? 0 });
    }
    return entries;
  }
  async list(path = ".", options: { recursive?: boolean; maxEntries?: number } = {}) {
    const start = performance.now();
    try {
      const directory = this.path(path);
      const entries = this.metadata();
      if (entries.get(directory)?.kind !== "directory") throw new Error(`not a directory: ${directory}`);
      const found = [...entries].filter(([name]) => name !== directory && name.startsWith(`${directory}/`)
        && (options.recursive || !name.slice(directory.length + 1).includes("/")))
        .map(([name, entry]) => ({ path: name, kind: entry.kind, ...(entry.kind === "file" ? { size: entry.size } : {}) }));
      if (found.length > (options.maxEntries ?? Infinity)) throw new Error("shell workspace listing limit exceeded");
      return found;
    } finally { this.onHydrate(performance.now() - start); }
  }
  async readFile(path: string): Promise<Uint8Array> {
    const target = this.path(path);
    if (this.#stage.has(target)) {
      const staged = this.#stage.get(target);
      if (staged?.kind !== "file") throw new Error(`no such file: ${target}`);
      return new Uint8Array(staged.data);
    }
    const row = this.storage.sql.exec<{ data: ArrayBuffer }>(
      "SELECT data FROM bash_files WHERE path = ? AND kind = 'file'", target,
    ).toArray()[0];
    if (!row) throw new Error(`no such file: ${target}`);
    return new Uint8Array(row.data);
  }
  async writeFile(path: string, contents: string | ArrayBuffer | ArrayBufferView): Promise<void> {
    const target = this.path(path);
    if (target === root) throw new Error("cannot write shell root");
    const entries = this.metadata();
    if (entries.get(target)?.kind === "directory") throw new Error(`is a directory: ${target}`);
    this.parents(target, entries);
    const bytes = typeof contents === "string" ? encoder.encode(contents)
      : ArrayBuffer.isView(contents) ? new Uint8Array(contents.buffer, contents.byteOffset, contents.byteLength) : new Uint8Array(contents);
    this.#stage.set(target, { kind: "file", data: new Uint8Array(bytes) });
  }
  private parents(path: string, entries: Map<string, Metadata>): void {
    let current = root;
    for (const part of path.slice(root.length + 1).split("/").slice(0, -1)) {
      current += `/${part}`;
      if (entries.get(current)?.kind === "file") throw new Error(`not a directory: ${current}`);
      if (!entries.has(current)) { this.#stage.set(current, { kind: "directory", data: null }); entries.set(current, { kind: "directory", size: 0 }); }
    }
  }
  async mkdir(path: string): Promise<void> {
    const target = this.path(path);
    if (target === root) return;
    const entries = this.metadata();
    this.parents(`${target}/child`, entries);
    if (entries.get(target)?.kind === "file") throw new Error(`not a directory: ${target}`);
    this.#stage.set(target, { kind: "directory", data: null });
  }
  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    const target = this.path(path);
    if (target === root) throw new Error("cannot remove shell root");
    const entries = this.metadata();
    if (!entries.has(target)) throw new Error(`no such path: ${target}`);
    const descendants = [...entries.keys()].filter(name => name.startsWith(`${target}/`));
    if (descendants.length && !options.recursive) throw new Error(`directory not empty: ${target}`);
    for (const name of [target, ...descendants]) this.#stage.set(name, null);
  }
  flush(): void {
    if (!this.#stage.size) return;
    this.storage.transactionSync(() => {
      for (const [path, entry] of this.#stage) {
        if (entry === null) this.storage.sql.exec("DELETE FROM bash_files WHERE path = ?", path);
        else this.storage.sql.exec(
          "INSERT INTO bash_files (path, kind, data) VALUES (?, ?, ?) ON CONFLICT(path) DO UPDATE SET kind=excluded.kind, data=excluded.data",
          path, entry.kind, entry.data,
        );
      }
    });
    this.#stage.clear();
  }
  discard(): void { this.#stage.clear(); }
}

/** Allocate neither the SQLite VFS nor the interpreter until a shell call. */
export function createJustBashTool(storage: DurableObjectStorage, onPhase?: BashPhaseObserver): NamedTool {
  let filesystem: SqliteWorkspace | undefined;
  let runtime: Promise<Awaited<ReturnType<typeof justBash>>> | undefined;
  let tail: Promise<unknown> = Promise.resolve();
  let observeHydrate: (durationMs: number) => void = () => {};
  const ready = () => runtime ??= justBash({
    filesystem: filesystem ??= new SqliteWorkspace(storage, duration => observeHydrate(duration)),
    refreshFilesystemBeforeExec: true, lazyInitialize: true, network: false,
    loadInterpreter: () => import("./just-bash-lazy.mjs"),
  });
  const phase = (context: ToolContext, name: BashPhase, durationMs: number) => {
    try { onPhase?.(context, name, durationMs); }
    catch { /* Observational telemetry cannot lose a shell result. */ }
  };
  return {
    name: "exec_command",
    description: "Run a one-shot in-process Bash command in the agent's durable /brain workspace. No native process, PTY, sandbox escalation, or network.",
    parameters: { type: "object", required: ["cmd"], properties: {
      cmd: { type: "string" }, workdir: { type: "string" }, max_output_tokens: { type: "integer" },
    }, additionalProperties: false },
    handler(input: unknown, context: ToolContext) {
      const execute = async () => {
        const setupStart = performance.now();
        let executionStart = setupStart;
        let hydrated = false;
        try {
          const shell = await ready();
          // The interpreter is imported by the generic tool handler, then
          // its aroundExecute hook calls list() once to refresh VFS metadata.
          observeHydrate = duration => {
            if (hydrated) return;
            hydrated = true;
            phase(context, "setup", Math.max(0, performance.now() - setupStart - duration));
            phase(context, "vfs_hydrate", duration);
            executionStart = performance.now();
          };
          try { return await shell.tool.handler(input, context); }
          finally {
            if (!hydrated) {
              phase(context, "setup", performance.now() - setupStart);
              phase(context, "vfs_hydrate", 0);
              executionStart = performance.now();
            }
            phase(context, "execute", performance.now() - executionStart);
            const flushStart = performance.now();
            try { filesystem!.flush(); }
            catch (error) { filesystem!.discard(); throw error; }
            finally { phase(context, "vfs_flush", performance.now() - flushStart); }
          }
        } finally { observeHydrate = () => {}; }
      };
      const result = tail.then(execute, execute);
      tail = result.then(() => undefined, () => undefined);
      return result;
    },
  };
}
