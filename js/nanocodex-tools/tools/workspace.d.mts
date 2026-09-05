import type { ToolMap, Workspace, WorkspaceEntry } from "./types.mjs";

export type { Workspace, WorkspaceEntry } from "./types.mjs";

export type WorkspaceBackend = {
  list(path: string, options: { recursive: boolean; maxEntries: number }): Promise<readonly WorkspaceEntry[]>;
  readFile(path: string): Promise<Uint8Array | ArrayBuffer | ArrayBufferView>;
  writeFile(path: string, contents: Uint8Array): Promise<void>;
  remove(path: string, options: { recursive: boolean }): Promise<void>;
  mkdir(path: string): Promise<void>;
};

export function createWorkspace(options: {
  backend: WorkspaceBackend;
  root?: string | undefined;
}): Workspace;

export function tools(workspace: Workspace, options?: {
  maxEntries?: number | undefined;
  maxReadBytes?: number | undefined;
  maxWriteBytes?: number | undefined;
}): ToolMap;

export function normalizeRelativePath(path: string): string;
export function resolveWorkspacePath(root: string, path: string): string;
