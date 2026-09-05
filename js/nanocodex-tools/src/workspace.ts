import type { Workspace, WorkspaceEntry } from "../tools/types.mjs";

const ROOT = "/workspace";
const DEFAULT_MAX_ENTRIES = 2_000;
const DEFAULT_MAX_FILE_BYTES = 64 * 1024 * 1024;

type ComputerDirent = Readonly<{
  name: string;
  size: number;
  mtime: number;
  isFile: boolean;
  isDirectory: boolean;
  isSymbolicLink: boolean;
}>;

type ComputerStat = Readonly<{
  size: number;
  isFile: boolean;
  isDirectory: boolean;
  isSymbolicLink: boolean;
}>;

/** Minimal storage interface needed to expose a persistent Nanocodex workspace. */
export type WorkspaceStorageClient = Readonly<{
  fs: {
    lstat(path: string): Promise<ComputerStat>;
    readdir(
      path: string,
      options?: { limit?: number | undefined; offset?: number | undefined },
    ): Promise<ComputerDirent[]>;
    readFile(
      path: string,
      options?: { byteOffset?: number | undefined; byteLength?: number | undefined },
    ): Promise<ReadableStream<Uint8Array>>;
    writeFile(path: string, contents: Uint8Array): Promise<void>;
    mkdir(path: string, options?: { recursive?: boolean | undefined }): Promise<void>;
    rm(path: string, options?: {
      recursive?: boolean | undefined;
      force?: boolean | undefined;
    }): Promise<void>;
  };
}>;

/**
 * Adapts a persistent filesystem client to Nanocodex's host-generic workspace
 * contract. Callers mount the returned handle into a bounded tool runtime.
 */
export async function createWorkspaceFilesystem(
  workspace: WorkspaceStorageClient,
  options: { maxFileBytes?: number | undefined } = {},
): Promise<Workspace> {
  const maxFileBytes = positiveInteger(
    options.maxFileBytes,
    DEFAULT_MAX_FILE_BYTES,
    "maxFileBytes",
  );
  await ensureRoot(workspace.fs);
  return createFilesystem(workspace.fs, maxFileBytes);
}

function createFilesystem(
  fs: WorkspaceStorageClient["fs"],
  maxFileBytes: number,
): Workspace {
  return Object.freeze({
    root: ROOT,
    async list(path = ".", options = {}) {
      const directory = resolvePath(path);
      const recursive = options.recursive === true;
      const maxEntries = positiveInteger(options.maxEntries, DEFAULT_MAX_ENTRIES, "maxEntries");
      await requireSafePath(fs, directory, "directory");
      const entries: WorkspaceEntry[] = [];
      const pending = [directory];

      while (pending.length > 0) {
        const current = pending.shift()!;
        const remaining = maxEntries - entries.length;
        const children = await fs.readdir(current, { limit: remaining + 1, offset: 0 });
        if (children.length > remaining) {
          throw new RangeError(`workspace listing exceeds ${maxEntries} entries`);
        }
        for (const child of children) {
          const childPath = resolveChild(current, child.name);
          requireSupportedEntry(child, childPath);
          entries.push(Object.freeze({
            kind: child.isDirectory ? "directory" : "file",
            modifiedAt: child.mtime,
            path: childPath,
            ...(child.isFile ? { size: child.size } : {}),
          }));
          if (recursive && child.isDirectory) pending.push(childPath);
        }
        if (!recursive) break;
      }

      return entries.sort((left, right) =>
        left.path < right.path ? -1 : left.path > right.path ? 1 : 0
      );
    },
    async readFile(path) {
      const target = resolvePath(path);
      const stat = await requireSafePath(fs, target, "file");
      if (stat.size > maxFileBytes) {
        throw new RangeError(`workspace file exceeds the ${maxFileBytes}-byte read bound`);
      }
      const stream = await fs.readFile(target, { byteOffset: 0, byteLength: stat.size + 1 });
      return readExactlyBounded(stream, stat.size, maxFileBytes);
    },
    async writeFile(path, contents) {
      const target = resolvePath(path);
      if (target === ROOT) throw new Error("cannot write the workspace root as a file");
      const bytes = toBytes(contents);
      if (bytes.byteLength > maxFileBytes) {
        throw new RangeError(`workspace file exceeds the ${maxFileBytes}-byte write bound`);
      }
      const parent = parentPath(target);
      await requireSafeAncestors(fs, parent);
      await fs.mkdir(parent, { recursive: true });
      await requireSafePath(fs, parent, "directory");
      const existing = await optionalLstat(fs, target);
      if (existing) requireSafeStat(existing, target, "file");
      await fs.writeFile(target, bytes);
    },
    async mkdir(path) {
      const target = resolvePath(path);
      await requireSafeAncestors(fs, target);
      await fs.mkdir(target, { recursive: true });
      await requireSafePath(fs, target, "directory");
    },
    async remove(path, options = {}) {
      const target = resolvePath(path);
      if (target === ROOT) throw new Error("cannot remove the workspace root");
      await requireSafePath(fs, target);
      await fs.rm(target, { recursive: options.recursive === true, force: false });
    },
  });
}

async function ensureRoot(fs: WorkspaceStorageClient["fs"]): Promise<void> {
  const existing = await optionalLstat(fs, ROOT);
  if (!existing) await fs.mkdir(ROOT, { recursive: true });
  await requireSafePath(fs, ROOT, "directory");
}

async function requireSafeAncestors(
  fs: WorkspaceStorageClient["fs"],
  target: string,
): Promise<void> {
  for (const candidate of pathPrefixes(target)) {
    const stat = await optionalLstat(fs, candidate);
    if (!stat) return;
    requireSafeStat(stat, candidate, "directory");
  }
}

async function requireSafePath(
  fs: WorkspaceStorageClient["fs"],
  target: string,
  kind?: "directory" | "file",
): Promise<ComputerStat> {
  let result: ComputerStat | undefined;
  for (const candidate of pathPrefixes(target)) {
    result = await fs.lstat(candidate);
    requireSafeStat(result, candidate, candidate === target ? kind : "directory");
  }
  if (!result) throw new Error(`invalid workspace path: ${target}`);
  return result;
}

function requireSafeStat(
  stat: ComputerStat,
  path: string,
  kind?: "directory" | "file",
): void {
  if (stat.isSymbolicLink) {
    throw new Error(`symbolic links are not supported by the Nanocodex workspace: ${path}`);
  }
  if (!stat.isFile && !stat.isDirectory) {
    throw new Error(`unsupported workspace entry: ${path}`);
  }
  if (kind === "directory" && !stat.isDirectory) throw new Error(`not a directory: ${path}`);
  if (kind === "file" && !stat.isFile) throw new Error(`not a file: ${path}`);
}

function requireSupportedEntry(entry: ComputerDirent, path: string): void {
  requireSafeStat(entry, path);
}

async function optionalLstat(
  fs: WorkspaceStorageClient["fs"],
  path: string,
): Promise<ComputerStat | undefined> {
  try {
    return await fs.lstat(path);
  } catch (error) {
    if (isMissing(error)) return undefined;
    throw error;
  }
}

function pathPrefixes(path: string): string[] {
  if (path === ROOT) return [ROOT];
  const relative = path.slice(ROOT.length + 1);
  const prefixes = [ROOT];
  let current = ROOT;
  for (const segment of relative.split("/")) {
    current += `/${segment}`;
    prefixes.push(current);
  }
  return prefixes;
}

function resolvePath(path: string): string {
  if (typeof path !== "string") throw new TypeError("workspace path must be a string");
  if (path.includes("\0")) throw new TypeError("workspace path cannot contain NUL");
  if (path.includes("\\")) throw new TypeError("workspace paths must use forward slashes");

  let relative = path;
  if (path.startsWith("/")) {
    if (path !== ROOT && !path.startsWith(`${ROOT}/`)) {
      throw new Error(`workspace path must stay within ${ROOT}`);
    }
    relative = path.slice(ROOT.length);
  }
  const segments: string[] = [];
  for (const segment of relative.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") throw new Error(`workspace path must stay within ${ROOT}`);
    segments.push(segment);
  }
  return segments.length === 0 ? ROOT : `${ROOT}/${segments.join("/")}`;
}

function resolveChild(parent: string, name: string): string {
  if (!name || name.includes("/") || name.includes("\\") || name === "." || name === "..") {
    throw new Error(`invalid workspace entry name: ${JSON.stringify(name)}`);
  }
  return resolvePath(`${parent}/${name}`);
}

function parentPath(path: string): string {
  return path.slice(0, path.lastIndexOf("/")) || ROOT;
}

async function readExactlyBounded(
  stream: ReadableStream<Uint8Array>,
  expectedBytes: number,
  maxBytes: number,
): Promise<Uint8Array> {
  const reader = stream.getReader();
  const output = new Uint8Array(expectedBytes);
  let offset = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!(value instanceof Uint8Array)) throw new TypeError("workspace read returned non-byte data");
      if (offset + value.byteLength > expectedBytes || offset + value.byteLength > maxBytes) {
        await reader.cancel("workspace file changed or exceeded its read bound");
        throw new RangeError(`workspace file changed or exceeds the ${maxBytes}-byte read bound`);
      }
      output.set(value, offset);
      offset += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (offset !== expectedBytes) {
    throw new Error(`workspace file changed while reading: expected ${expectedBytes} bytes, received ${offset}`);
  }
  return output;
}

function toBytes(value: string | ArrayBuffer | ArrayBufferView): Uint8Array {
  if (typeof value === "string") return new TextEncoder().encode(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new TypeError("workspace contents must be a string or byte array");
}

function positiveInteger(value: unknown, fallback: number, name: string): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new TypeError(`${name} must be positive`);
  }
  return value as number;
}

function isMissing(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const value = error as { code?: unknown; message?: unknown };
  return value.code === "ENOENT"
    || (typeof value.message === "string" && /ENOENT|not found|no such/i.test(value.message));
}
