import type { Workspace } from "../../runtime/workspace.mjs";
import type { NamedTool } from "../../types.mjs";
import type { JustBashDescriptor } from "../bash.mjs";
import type { DatasetOptions, JsonToolOptions } from "../index.mjs";

export type BrowserThread = Readonly<{
  id: string;
  workspaceName: string;
  repositoryName: string;
  branch: "nanocodex";
  remoteUrl: string;
  shareUrl: string;
}>;

export type ThreadGitStatus = Readonly<{
  branch: "nanocodex";
  head?: string | undefined;
  changes: string[];
  remoteUrl: string;
}>;

export type OpfsFileStat = Readonly<{
  size: number;
  mtimeMs: number;
  isFile(): boolean;
  isDirectory(): boolean;
  isSymbolicLink(): false;
}>;

export type OpfsDirectoryEntry = Readonly<{
  name: string;
  isFile(): boolean;
  isDirectory(): boolean;
  isSymbolicLink(): false;
}>;

export type OpfsGitFs = Readonly<{
  promises: Readonly<{
    readFile(path: string): Promise<Uint8Array>;
    readFile(path: string, options: string | { encoding: string; maxBytes?: number }): Promise<string>;
    readFile(path: string, options: { maxBytes: number; encoding?: undefined }): Promise<Uint8Array>;
    writeFile(path: string, value: string | Uint8Array): Promise<void>;
    appendFile(path: string, value: string | Uint8Array): Promise<void>;
    unlink(path: string): Promise<void>;
    readdir(path: string): Promise<string[]>;
    readdirWithFileTypes(path: string): Promise<OpfsDirectoryEntry[]>;
    mkdir(path: string): Promise<void>;
    rmdir(path: string): Promise<void>;
    rm(path: string, options?: { recursive?: boolean }): Promise<void>;
    stat(path: string): Promise<OpfsFileStat>;
    lstat(path: string): Promise<OpfsFileStat>;
    readlink(path: string): Promise<never>;
    symlink(target: string, path: string): Promise<never>;
  }>;
}>;

export type BrowserCommandResult = Readonly<{
  output: string;
  wall_time_seconds: number;
  exit_code: number;
  original_token_count?: number | undefined;
}>;

export type BrowserBashResult = Readonly<{
  stdout: string;
  stderr: string;
  exitCode: number;
  env: Record<string, string>;
}>;

export type BrowserBash = {
  exec(
    script: string,
    options?: Readonly<{
      cwd?: string | undefined;
      env?: Record<string, string> | undefined;
      replaceEnv?: boolean | undefined;
      rawScript?: boolean | undefined;
      stdin?: string | undefined;
      stdinKind?: "text" | "bytes" | undefined;
      signal?: AbortSignal | undefined;
      args?: string[] | undefined;
    }>,
  ): Promise<BrowserBashResult>;
};

export type BrowserShellStat = Readonly<{
  isFile: boolean;
  isDirectory: boolean;
  isSymbolicLink: boolean;
  mode: number;
  size: number;
  mtime: Date;
}>;

export type BrowserShellFileSystem = {
  readonly mutationVersion: number;
  refreshPaths(): Promise<void>;
  recordExternalWrite(path: string): void;
  recordExternalRemove(path: string): void;
  recordRepositoryMutation(): void;
  readFile(path: string, options?: string | { encoding?: string }): Promise<string>;
  readFileBytes(path: string): Promise<string>;
  readFileBuffer(path: string): Promise<Uint8Array>;
  writeFile(path: string, content: string | Uint8Array, options?: string): Promise<void>;
  appendFile(path: string, content: string | Uint8Array, options?: string): Promise<void>;
  exists(path: string): Promise<boolean>;
  stat(path: string): Promise<BrowserShellStat>;
  mkdir(path: string): Promise<void>;
  readdir(path: string): Promise<string[]>;
  readdirWithFileTypes(path: string): Promise<OpfsDirectoryEntry[]>;
  rm(path: string, options?: { force?: boolean; recursive?: boolean }): Promise<void>;
  cp(src: string, dest: string, options?: { recursive?: boolean }): Promise<void>;
  mv(src: string, dest: string): Promise<void>;
  resolvePath(base: string, path: string): string;
  getAllPaths(): string[];
  chmod(path: string, mode: number): Promise<void>;
  symlink(target: string, linkPath: string): Promise<never>;
  link(existingPath: string, newPath: string): Promise<never>;
  readlink(path: string): Promise<never>;
  lstat(path: string): Promise<BrowserShellStat>;
  realpath(path: string): Promise<string>;
  utimes(path: string, atime: Date | number, mtime: Date | number): Promise<void>;
};

export type BrowserOptions = Readonly<{
  threadId: string;
  origin?: string | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  headers?: HeadersInit | undefined;
  installFetch?: boolean | undefined;
  /** Exposes a structured request that an app can render as an account-connect button. */
  accountConnectionRequests?: boolean | undefined;
  accountInfo?: Readonly<{
    endpoint?: string | undefined;
    requireAuthorization?: boolean | undefined;
  }> | undefined;
  web?: JsonToolOptions | undefined;
  images?: JsonToolOptions | undefined;
  dataset?: DatasetOptions | undefined;
  recentImages?(sessionId: string, count: number): string[];
  rememberImage?(sessionId: string, imageUrl: string): void;
}>;

export type BrowserTools = Readonly<{
  filesystem: Workspace;
  instructions: string;
  projectInstructions?: string | undefined;
  tools: readonly NamedTool[];
}>;

/** Thread-scoped OPFS, Git, shell, and module state reusable across Agent starts. */
export type PreparedBrowser = Readonly<{
  origin: string;
  threadId: string;
}>;

/** Opens a persistent OPFS workspace and composes its WASM-backed tools. */
export function browser(options: BrowserOptions): Promise<BrowserTools>;
export function prepareBrowser(
  options: Pick<BrowserOptions, "threadId" | "origin">,
): Promise<PreparedBrowser>;
export function bindBrowser(prepared: PreparedBrowser, options: BrowserOptions): BrowserTools;

export function browserThread(threadId: string, origin: string): BrowserThread;
export function getBrowserThread(): BrowserThread;
export function selectBrowserThread(threadId: string): BrowserThread;
export function openKernelWorkspace(): Promise<Workspace>;
export function openThreadWorkspace(threadId: string): Promise<Workspace>;
export function subscribeThreadWorkspaceChanges(threadId: string, listener: () => void): () => void;

export function openOpfsWorkspaceRoot(workspaceName: string): Promise<FileSystemDirectoryHandle>;
export function openOpfsGitFs(workspaceName: string): Promise<OpfsGitFs>;
export function createOpfsGitFs(root: FileSystemDirectoryHandle): OpfsGitFs;

export function initializeThreadGit(thread: BrowserThread): Promise<ThreadGitStatus>;
export function inspectThreadGit<T>(thread: BrowserThread, inspect: (fs: OpfsGitFs) => Promise<T>): Promise<T>;
export function threadGitStatus(thread: BrowserThread): Promise<ThreadGitStatus>;
export function commitAndPushThread(
  thread: BrowserThread,
  message?: string,
  notificationSource?: string,
): Promise<ThreadGitStatus>;
export function pullThread(thread: BrowserThread, notificationSource?: string): Promise<ThreadGitStatus>;
export function subscribeThreadGitChanges(thread: BrowserThread, listener: (source?: string) => void): () => void;
export function notifyThreadGitChanged(thread: BrowserThread, source?: string): void;
export function withThreadGitLock<T>(
  thread: BrowserThread,
  operation: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T>;

export function loadBrowserProjectInstructions(rawFs: OpfsGitFs): Promise<string | undefined>;
export function validateBrowserArtifactSource(source: string): Promise<void>;
export function createBrowserBash(
  rawFs: OpfsGitFs,
  thread: BrowserThread,
  options?: Record<string, unknown>,
): Promise<{
  bash: BrowserBash;
  descriptor: JustBashDescriptor;
  filesystem: BrowserShellFileSystem;
  instructions: string;
  tool: NamedTool;
  exec(
    input: Record<string, unknown>,
    context?: { signal?: AbortSignal },
  ): Promise<BrowserCommandResult>;
}>;
