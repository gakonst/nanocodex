import type {
  AgentLifecycle,
  AgentOptions,
  CodeEvaluator,
  CodeEffectJournal,
  DefaultAgent,
  DurabilityStore,
  McpServers,
  ToolConfiguration,
} from "../types.mjs";
import type { ManagedTransport, ResponsesTransport } from "./Transport.mjs";
import type { Tool as SubagentTool } from "../runtime/subagents.mjs";
import type { Workspace } from "./workspace.mjs";
import type { Tools } from "../tools/Tools.mjs";

export type Agent = DefaultAgent;
type ToolExposureOptions =
  | { mcp?: false | undefined; toolMode?: "code" | "direct" | undefined }
  | { mcp: McpServers; toolMode?: "code" | undefined };

/** Creates a Node-hosted Rust/WASM Agent. */
export function create(options: import('../runtime/claude.mjs').Options & { harness: 'claude' }): Promise<import('../runtime/claude.mjs').Agent>;
export function create(options: create.ManagedOptions): Promise<AgentLifecycle>;
export function create(options: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type ManagedOptions = Readonly<{
    transport: ManagedTransport;
    tools?: Tools | undefined;
  }>;
  type Options = AgentOptions & ToolExposureOptions & {
    codeEvaluator?: CodeEvaluator | undefined;
    /** Opt-in durable direct-tool and nested Code Mode receipts for safe cold recovery. */
    codeEffectJournal?: CodeEffectJournal | undefined;
    /** Caller-owned rooted filesystem mounted through standard workspace tools. */
    filesystem?: Workspace | undefined;
    module?: unknown;
    transport: ResponsesTransport;
  } & (
    | {
      durability?: undefined;
      durabilityId?: undefined;
      tools?: ToolConfiguration<SubagentTool> | undefined;
    }
    | {
      durability: DurabilityStore;
      durabilityId: string;
      /** Subagent identities, mailboxes, and execution recover on this durability store. */
      tools?: ToolConfiguration<SubagentTool> | undefined;
    }
  );
  type ReturnType = Agent;
}
