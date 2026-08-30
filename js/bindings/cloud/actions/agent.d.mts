import type { TurnUsage } from "../../types.mjs";
import type { Client } from "../Client.mjs";
import type { Connection, ConnectAgent } from "../types.mjs";

export function create(
  client: Client,
  options: create.Options,
): Promise<ConnectAgent>;

export declare namespace create {
  type Options = Readonly<{
    connection: Connection;
    /** Reverse-attach the embedding page's WebMCP tools to this durable Agent. */
    webMcp?: true | import("../../webmcp/WebMcp.mjs").WebMcpProviderOptions | undefined;
  }>;
  type ReturnType = ConnectAgent;
  type TurnUsageResult = TurnUsage;
}
