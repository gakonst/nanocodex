export const productionConnectApiOrigin: "https://nanocodex-connect-api.gakonst.workers.dev";

export type RegisteredApp = Readonly<{
  id: string;
  name: string;
  origin: string;
}>;

export function registeredApp(
  embeddingOrigin: string,
  appId: string,
  dialogUrl: string,
  isTopLevel: boolean,
  allowDynamicPopup?: boolean,
): RegisteredApp;
export function isPopupPresentation(dialogUrl: string, isTopLevel: boolean): boolean;
export function signedAppResources(resources: unknown, app: RegisteredApp): readonly unknown[];
export type ConnectPolicy = Readonly<{ chatGptCredentialImport: boolean }>;
export function parseConnectPolicy(resources: unknown): ConnectPolicy;
export function connectApiOrigin(auth: unknown, dialogOrigin: string): string;
export function sanitizeWalletResult(result: unknown): Readonly<{
  accounts: readonly Readonly<{
    address?: unknown;
    capabilities: Readonly<Record<string, unknown> & {
      auth: Readonly<{ approval_id: string }>;
    }>;
  }>[];
}> & Record<string, unknown>;
export function sanitizeCliWalletResult(result: unknown): Readonly<{
  accounts: readonly Readonly<{
    address: `0x${string}`;
    capabilities: Readonly<{
      keyAuthorization: Readonly<Record<string, unknown>>;
      personalSign: Readonly<{ keyAuthorization: `0x${string}` }>;
      auth: Readonly<{ approval_id: string }>;
    }> | Readonly<{
      auth: Readonly<{ approval_id: string; mode: "hosted" }>;
    }>;
  }>[];
}>;
export function appVisibilityPermissions(resources: unknown): readonly Readonly<{
  resource: string;
  label: "Reply" | "Actions" | "History" | "Traces" | "Hosted history" | "Memory read" | "Memory write";
  detail: string;
}>[];
export function accountLoginCapabilities(accounts: unknown): Readonly<
  | { method: "login"; credentialId: readonly string[] }
  | { method: "login" }
>;
export function connectorApprovalDisposition(
  requestedConnectors: unknown,
  statuses: unknown,
): "wait" | "respond";
export function chatGptConnectorDisposition(value: unknown): "connected" | "device" | "invalid";
export function focusedConnectorFromResources(
  resources: unknown,
  requestedConnectors: unknown,
): "chatgpt" | "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks" | "gdocs" | "gsheets" | "gslides" | "gcontacts" | "slack" | "x" | undefined;
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
export function mcpConnectionsFromWire(value: unknown): readonly McpConnection[];
export function focusedMcpConnection(
  value: unknown,
  connections: unknown,
): string | undefined;
export function mcpConnectionApprovalDisposition(
  requestedConnections: unknown,
  connections: unknown,
): "wait" | "respond";
export type McpCallbackContinuation = Readonly<{
  version: 1;
  expiresAt: number;
  requestId: string;
  apiUrl: string;
  accountAddress: `0x${string}`;
  token: string;
  requestedConnectors: readonly ("chatgpt" | "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks" | "gdocs" | "gsheets" | "gslides" | "gcontacts" | "slack" | "x")[];
  requestedMcpConnections: readonly McpConnection[];
  connectorStatuses: Readonly<Record<string, Readonly<{
    connected: boolean;
    connections: readonly Readonly<{
      id: string;
      label: string;
      account_id?: string | undefined;
      capabilities: readonly ("chatgpt" | "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks" | "gdocs" | "gsheets" | "gslides" | "gcontacts" | "slack" | "x")[];
    }>[];
    account_id?: string | undefined;
    connection_id?: string | undefined;
    label?: string | undefined;
  }>>>;
  result: ReturnType<typeof sanitizeCliWalletResult>;
}>;
export function createMcpCallbackContinuation(value: unknown, now?: number): McpCallbackContinuation;
export function restoreMcpCallbackContinuation(
  value: unknown,
  expected: Readonly<{
    requestId: string;
    apiUrl: string;
    returnedConnector?: "github" | "google" | "slack" | "x" | undefined;
    returnedMcpConnection?: string | undefined;
    requestedConnectors: readonly string[];
    requestedMcpConnections: readonly McpConnection[];
  }>,
  now?: number,
): McpCallbackContinuation;
export function isLocalDevelopmentOrigin(value: string): boolean;
export function usesBrowserLocalWebAuthn(value: string): boolean;
export function deviceMcpReturnPath(value: string): string;
