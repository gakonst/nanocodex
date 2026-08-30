import type { Config } from "../browser/config.mjs";
import type { ConnectAgent, Connection } from "../cloud/types.mjs";
import type { WebMcpManifest } from "../webmcp/WebMcp.mjs";

export const automaticWebMcpConfig: Config<ConnectAgent>;
export function automaticWebMcpConnection(): Connection | undefined;

export type AutomaticWebMcpReady = Readonly<{
  agent: ConnectAgent;
  connection: Connection;
  manifest: WebMcpManifest;
  publication: Readonly<{ tools: readonly string[]; close(): void }>;
}>;

export function startAutomaticWebMcp(options?: Readonly<{
  endpoint?: string | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  client?: import("../cloud/Client.mjs").Client | undefined;
}>): Promise<AutomaticWebMcpReady>;

export function prepareAutomaticWebMcp(options?: Readonly<{
  endpoint?: string | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  client?: import("../cloud/Client.mjs").Client | undefined;
  force?: boolean | undefined;
}>): Promise<AutomaticWebMcpReady>;
