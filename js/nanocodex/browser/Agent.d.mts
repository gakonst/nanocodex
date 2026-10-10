import type {
  AgentLifecycle,
  AgentOptions,
  DefaultAgent,
  ExecutionEnvironment,
} from "../types.mjs";
import type { ManagedTransport, WorkerTransport } from "./Transport.mjs";
import type { Tools } from "../tools/Tools.mjs";

export type Agent = DefaultAgent;

type WorkerMcpServer = Readonly<{
  url?: string | URL | undefined;
  description?: string | undefined;
  headers?: Readonly<Record<string, string>> | readonly (readonly [string, string])[] | undefined;
  enabledTools?: readonly string[] | undefined;
  disabledTools?: readonly string[] | undefined;
  supportsParallelToolCalls?: boolean | undefined;
  parallelTools?: readonly string[] | undefined;
  startupTimeoutMs?: number | undefined;
  timeoutMs?: number | undefined;
}>;
type WorkerMcpServers = Readonly<Record<string, string | URL | WorkerMcpServer>>;
type WorkerToolExposureOptions = {
  mcp?: WorkerMcpServers | false | undefined;
  toolMode?: "code-only" | undefined;
};

/** Creates a Rust/WASM Agent in a package-owned browser module Worker. */
export function create(options: create.ManagedOptions): Promise<AgentLifecycle>;
export function create(options?: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type ManagedOptions = Readonly<{
    transport: ManagedTransport;
    tools?: Tools | undefined;
  }>;
  /** Codex (OpenAI Responses) harness options. */
  type CodexOptions = Omit<AgentOptions, "beforeCompaction" | "harness"> & WorkerToolExposureOptions & {
    /** Precompiled browser module; WebAssembly modules are structured-clone-safe. */
    module?: WebAssembly.Module | undefined;
    /** Fixed browser workspace facts, including its AGENTS.md snapshot. */
    executionEnvironment?: ExecutionEnvironment | undefined;
    /** Defaults to the same-origin Nanocodex `/api/responses` proxy. */
    transport?: WorkerTransport | undefined;
    /** Stable OPFS/Git workspace identity for the default browser harness. */
    threadId?: string | undefined;
    /** Exposes provider authorization links in the default browser harness. */
    accountConnectionRequests?: boolean | undefined;
    /** Set false to keep this browser session out of the IndexedDB durability store. */
    durability?: false | undefined;
    /** Set false to omit the default OPFS, shell, web, image, plan, and artifact tools. */
    harness?: false | 'codex' | undefined;
  };
  /** Claude (Messages) harness options; see docs/CLAUDE_JAVASCRIPT.md. */
  type ClaudeOptions = import('../runtime/claude.mjs').Options & { harness: 'claude' };
  /** Options of either harness family, discriminated by `harness`; both return the same Agent. */
  type Options = CodexOptions | ClaudeOptions;
  type ReturnType = Agent;
}
