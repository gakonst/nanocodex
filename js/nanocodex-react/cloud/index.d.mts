import type {
  UseMutationOptions,
  UseMutationResult,
} from "@tanstack/react-query";
import type {
  Actions,
  Client,
  Connection,
  ConnectAgent,
  Dialog,
  McpConnection,
} from "nanocodex/connect";
import type { ReactNode } from "react";
import type { Agent as StructuralAgent } from "../agent/index.mjs";

export type ConnectionStatus = "disconnected" | "connecting" | "connected";

export type ConnectionSnapshot<connection = unknown, agent = unknown> =
  | Readonly<{
    agent: undefined;
    connection: undefined;
    status: "disconnected";
    isConnected: false;
    isConnecting: false;
    isDisconnected: true;
  }>
  | Readonly<{
    agent: undefined;
    connection: undefined;
    status: "connecting";
    isConnected: false;
    isConnecting: true;
    isDisconnected: false;
  }>
  | Readonly<{
    agent: agent | undefined;
    connection: connection;
    status: "connected";
    isConnected: true;
    isConnecting: false;
    isDisconnected: false;
  }>;

export type Config<client extends Client.Client = Client.Client> = Readonly<{
  client: client;
  getState(): ConnectionSnapshot<Connection, ConnectAgent>;
  subscribe(listener: () => void): () => void;
}>;

export type CreateConfigParameters<client extends Client.Client = Client.Client> = Readonly<{
  client: client;
}>;

export function createConfig<const client extends Client.Client>(
  parameters: CreateConfigParameters<client>,
): Config<client>;

export function NanocodexProvider(props: Readonly<{
  children: ReactNode;
  config: Config;
}>): ReactNode;

export type ConfigParameter<config extends Config = Config> = Readonly<{
  config?: config | undefined;
}>;

export function useConfig<config extends Config = Config>(
  parameters?: ConfigParameter<config>,
): config;

export function useConnection<config extends Config = Config>(
  parameters?: ConfigParameter<config>,
): ReturnType<config["getState"]>;

/** Capability-bound Nanocodex agent injected after Connect approval. */
export function useAgent<config extends Config = Config>(
  parameters?: ConfigParameter<config>,
): ConnectAgent | undefined;

export type UseConnectMutationParameters<
  data,
  variables,
  error = Error,
  context = unknown,
> = ConfigParameter & Readonly<{
  mutation?: Omit<
    UseMutationOptions<data, error, variables, context>,
    "mutationFn" | "mutationKey"
  > | undefined;
}>;

export type UseConnectMutationReturnType<
  data,
  variables,
  error = Error,
  context = unknown,
> = UseMutationResult<data, error, variables, context>;

/** Compatibility alias for the canonical secret-free Connect MCP metadata. */
export type HostedMcpConnectionRequest = McpConnection;
export type HostedConnectOptions = Actions.connection.connect.Options;
export type HostedReconnectOptions = Omit<Actions.connection.reconnect.Options, "signal">;

export function useConnect<
  error = Error,
  context = unknown,
>(parameters?: UseConnectMutationParameters<
  Connection,
  HostedConnectOptions,
  error,
  context
>): UseConnectMutationReturnType<
  Connection,
  HostedConnectOptions,
  error,
  context
>;

export type ConnectAgentResult = Readonly<{
  agent: ConnectAgent;
  connection: Connection;
}>;

export type UseConnectAgentParameters<
  error = Error,
  context = unknown,
> = ConfigParameter & Readonly<{
  agent?: Omit<Actions.agent.create.Options, "connection"> | undefined;
  /** Fallback projection for a legacy session without a persisted connection projection. */
  reconnect?: HostedReconnectOptions | undefined;
  /** Validate and reopen the app-scoped durable grant session on mount. @default true */
  reconnectOnMount?: boolean | undefined;
  mutation?: Omit<
    UseMutationOptions<ConnectAgentResult, error, HostedConnectOptions, context>,
    "mutationFn" | "mutationKey"
  > | undefined;
}>;

export type UseConnectAgentReturnType<
  error = Error,
  context = unknown,
> = UseMutationResult<
  ConnectAgentResult,
  error,
  HostedConnectOptions,
  context
> & Readonly<{
  agent: ConnectAgent | undefined;
  connection: Connection | undefined;
  connectionStatus: ConnectionStatus;
  connect: UseMutationResult<ConnectAgentResult, error, HostedConnectOptions, context>["mutate"];
  connectAsync: UseMutationResult<ConnectAgentResult, error, HostedConnectOptions, context>["mutateAsync"];
}>;

export function useConnectAgent<
  error = Error,
  context = unknown,
>(parameters?: UseConnectAgentParameters<error, context>): UseConnectAgentReturnType<error, context>;

export type ConnectAgentSourceOptions = Readonly<{
  /** Whether the signed Connect grant permits retained conversation history. */
  history: boolean;
}>;

/** Normalizes a capability-bound Connect agent for nanocodex-react/agent. */
export function createConnectAgentSource(
  connectAgent: ConnectAgent,
  options: ConnectAgentSourceOptions,
): StructuralAgent;

export function useFund<
  error = Error,
  context = unknown,
>(parameters?: UseConnectMutationParameters<
  Awaited<Actions.machineUsd.fund.ReturnType>,
  Actions.machineUsd.fund.Options,
  error,
  context
>): UseConnectMutationReturnType<
  Awaited<Actions.machineUsd.fund.ReturnType>,
  Actions.machineUsd.fund.Options,
  error,
  context
>;

export function useCharge<
  error = Error,
  context = unknown,
>(parameters?: UseConnectMutationParameters<
  Awaited<Actions.mpp.charge.ReturnType>,
  Actions.mpp.charge.Options,
  error,
  context
>): UseConnectMutationReturnType<
  Awaited<Actions.mpp.charge.ReturnType>,
  Actions.mpp.charge.Options,
  error,
  context
>;

export function useLogoutAccount<
  error = Error,
  context = unknown,
>(parameters?: UseConnectMutationParameters<
  void,
  void,
  error,
  context
>): UseConnectMutationReturnType<
  void,
  void,
  error,
  context
>;

export function useRevokeGrant<
  error = Error,
  context = unknown,
>(parameters?: UseConnectMutationParameters<
  Awaited<Actions.grant.revoke.ReturnType>,
  Actions.grant.revoke.Options,
  error,
  context
>): UseConnectMutationReturnType<
  Awaited<Actions.grant.revoke.ReturnType>,
  Actions.grant.revoke.Options,
  error,
  context
>;

export type ConnectDialogRequest = Dialog.ConnectionRequest;

export type MachineUsdFundDialogRequest = Dialog.FundingRequest;
export type MemoryDialogInstance = Dialog.Instance;

export function NanocodexDialog(props: Readonly<{
  dialog: MemoryDialogInstance;
}>): ReactNode;
