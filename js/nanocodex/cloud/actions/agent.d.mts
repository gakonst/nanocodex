import type { NamedTool, ToolMap, TurnUsage } from "../../types.mjs";
import type { Base } from "../Client.mjs";
import type { Connection, ConnectAgent, HostConnection } from "../types.mjs";

export function create(
  client: Base,
  options: create.Options,
): Promise<ConnectAgent>;

export declare namespace create {
  type Options = Readonly<{
    connection: Connection | HostConnection;
    signal?: AbortSignal | undefined;
    tools?: ToolMap | readonly NamedTool[] | undefined;
  }>;
  type ReturnType = ConnectAgent;
  type TurnUsageResult = TurnUsage;
}
