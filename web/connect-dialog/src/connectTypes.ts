import type { Dialog } from "nanocodex/connect";

export type McpConnectionStatus =
  | "authorization_required"
  | "reauthorization_required"
  | "connected"
  | "disabled"
  | "revoked";

export type McpConnection = Readonly<{
  id: string;
  name: string;
  status: McpConnectionStatus;
}>;

type WalletRequestBase = Readonly<{
  appId: string;
  id: string;
  origin: string;
  rpc: Readonly<{ method: string; params?: unknown }>;
  confirmationCode?: string | undefined;
  requestedMcpConnections?: readonly McpConnection[] | undefined;
  focusMcpConnection?: string | undefined;
  returnedConnector?: "github" | "gmail" | "gdrive" | "x" | "whoop" | undefined;
  returnedConnectorResult?: "connected" | "cancelled" | "failed" | undefined;
  returnedMcpConnection?: string | undefined;
  returnedMcpResult?: "connected" | "cancelled" | "failed" | undefined;
}>;

export type WalletRequest =
  | WalletRequestBase & Readonly<{ type: "walletConnect" }>
  | WalletRequestBase & Readonly<{ type: "walletRevokeAccessKey" }>;

export type ConnectRequest =
  | WalletRequest
  | Dialog.FundingRequest
  | Readonly<{ id: string; message: string; type: "deviceError" }>
  | Readonly<{
      connectorName?: string | undefined;
      id: string;
      status: "approved" | "denied";
      type: "deviceComplete";
    }>;
