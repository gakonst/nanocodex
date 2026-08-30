import type {
  AgentLifecycle,
  AgentOptions,
  DefaultAgent,
  ExecutionEnvironment,
} from "../types.mjs";
import type { ManagedTransport, WorkerTransport } from "./Transport.mjs";
import type { Tools } from "../tools/Tools.mjs";
import type { WebMcpProviderOptions, WebMcpToolProvider } from "../webmcp/WebMcp.mjs";

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
type WorkerToolExposureOptions =
  | { mcp?: false | undefined; toolMode?: "code" | "direct" | undefined }
  | { mcp: WorkerMcpServers; toolMode?: "code" | undefined };

/** Creates a Rust/WASM Agent in a package-owned browser module Worker. */
export function create(options: create.ManagedOptions): Promise<AgentLifecycle>;
export function create(options?: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type ManagedOptions = Readonly<{
    transport: ManagedTransport;
    tools?: Tools | undefined;
    /** Attach the host page's WebMCP capabilities under its existing session. */
    webMcp?: true | WebMcpProviderOptions | WebMcpToolProvider | false | undefined;
  }>;
  type Options = AgentOptions & WorkerToolExposureOptions & {
    /** Precompiled browser module; WebAssembly modules are structured-clone-safe. */
    module?: WebAssembly.Module | undefined;
    /** Fixed browser workspace facts, including its AGENTS.md snapshot. */
    executionEnvironment?: ExecutionEnvironment | undefined;
    /** Defaults to the same-origin Nanocodex `/api/responses` proxy. */
    transport?: WorkerTransport | undefined;
    /** Stable OPFS/Git workspace identity for the default browser harness. */
    threadId?: string | undefined;
    /** Set false to keep this browser session out of the IndexedDB durability store. */
    durability?: false | undefined;
    /** Set false to omit the default OPFS, shell, web, image, plan, and artifact tools. */
    harness?: false | undefined;
    /** Discover WebMCP tools in the embedding page and bridge them into the Agent Worker. */
    webMcp?: true | WebMcpProviderOptions | WebMcpToolProvider | false | undefined;
  };
  type ReturnType = Agent;
}
