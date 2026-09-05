import type { Workspace } from "./types.mjs";

export type RepositoryWorkspaceFetch = (
  url: string,
  options?: Readonly<{
    method?: string | undefined;
    headers?: Headers | Record<string, string> | undefined;
    body?: Uint8Array | undefined;
    signal?: AbortSignal | undefined;
  }>,
) => Promise<Response | Readonly<{
  status: number;
  statusText?: string | undefined;
  headers?: Headers | Record<string, string> | undefined;
  body?: Uint8Array | AsyncIterable<Uint8Array> | undefined;
  url?: string | undefined;
  arrayBuffer?(): Promise<ArrayBuffer>;
}>>;

export type RepositoryWorkspaceRemote = Readonly<{
  name: string;
  url: string;
  branch: string;
}>;

export type RepositoryWorkspaceDescriptor = Readonly<{
  branch: string;
  directory: string;
  gitDirectory: string;
  head: string;
  markerPath: string;
  seedUrl: string;
  writableRemote?: RepositoryWorkspaceRemote | undefined;
}>;

/**
 * Materializes one immutable, generation-pinned Git seed into a package-owned
 * workspace child. A matching retained checkout is reopened without network
 * access or implicit refresh, preserving local worktree changes.
 */
export function materializeRepositoryWorkspace(options: Readonly<{
  workspace: Workspace;
  fetch: RepositoryWorkspaceFetch;
  /** Credential-free smart-HTTP URL ending in the exact lowercase SHA-1 `head`. */
  seedUrl: string;
  head: string;
  branch?: string | undefined;
  /** One safe child name beneath `workspace.root`; defaults to `repository`. */
  directory?: string | undefined;
  writableRemote?: Readonly<{
    url: string;
    name?: string | undefined;
    branch?: string | undefined;
  }> | undefined;
}>): Promise<RepositoryWorkspaceDescriptor>;
