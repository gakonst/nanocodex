import type { NamedTool, Workspace } from "./types.mjs";

export type JustBashFetch = (
  url: string,
  options?: Readonly<{
    method?: string | undefined;
    headers?: Headers | Record<string, string> | undefined;
    body?: string | undefined;
    followRedirects?: boolean | undefined;
    timeoutMs?: number | undefined;
    maxRedirects?: number | undefined;
    signal?: AbortSignal | undefined;
  }>,
) => Promise<Readonly<{
  status: number;
  statusText: string;
  headers: Record<string, string>;
  body: Uint8Array;
  url: string;
}>>;

export type JustBashCustomCommand = Readonly<{
  name: string;
  trusted?: boolean | undefined;
  execute(
    args: string[],
    context: unknown,
  ): Promise<Readonly<{ stdout: string; stderr: string; exitCode: number }>>;
}> | Readonly<{
  name: string;
  load(): Promise<JustBashCustomCommand>;
}>;

export type JustBashNetworkOptions = Readonly<{
  /** Explicitly permit credential-free HTTP(S) requests to arbitrary origins. */
  dangerouslyAllowFullInternetAccess?: boolean | undefined;
  /** Otherwise, allow only these exact origin/path prefixes. */
  allowedUrlPrefixes?: readonly string[] | undefined;
  /** Defaults to GET and HEAD in Just Bash. */
  allowedMethods?: readonly string[] | undefined;
}>;

export type JustBashRuntime = Readonly<{
  /**
   * The authoritative workspace handle while this runtime is mounted. All mutations must use
   * this handle so Bash's bounded metadata view remains synchronized.
   */
  filesystem: Workspace;
  /** Model instructions derived from the actual shell descriptor. */
  instructions: string;
  /** Exact command, workspace, limit, network, and escalation boundary. */
  descriptor: JustBashDescriptor;
  /** One-shot, cancellable `exec_command` tool backed by Just Bash. */
  tool: NamedTool;
}>;

export type JustBashDescriptor = Readonly<{
  shell: "nanocodex-just-bash";
  commands: readonly string[];
  customCommands: readonly string[];
  cwd: string;
  limits: Readonly<Record<string, number>>;
  network: Readonly<{ enabled: boolean; mode: string }>;
  pty: false;
  sessions: false;
  sandboxEscalation: false;
}>;

/**
 * Mounts a caller-owned workspace into an in-isolate Just Bash runtime.
 *
 * Do not mutate the source `filesystem` handle while the runtime is mounted. Use the returned
 * `filesystem` for every mutation so the shell's metadata view remains authoritative.
 */
export function justBash(options: {
  /** Caller-owned durable workspace. See `JustBashRuntime.filesystem` for mutation ownership. */
  filesystem: Workspace;
  /** Refresh metadata before each serialized command when another writer shares the storage. */
  refreshFilesystemBeforeExec?: boolean | undefined;
  executionTimeoutMs?: number | undefined;
  maxEntries?: number | undefined;
  maxOutputTokens?: number | undefined;
  network?: false | JustBashNetworkOptions | undefined;
  /** Stable host-owned name for the actual network boundary exposed in the descriptor. */
  networkMode?: string | undefined;
  /** Host-owned fetch boundary used by curl and app-owned commands. */
  fetch?: JustBashFetch | undefined;
  /** Application commands registered directly in the embedded interpreter. */
  customCommands?: readonly JustBashCustomCommand[] | undefined;
}): Promise<JustBashRuntime>;
