import type { Base, Client } from "../Client.mjs";
import type { Instance as PrincipalInstance } from "../Principal.mjs";
import type {
  CloudAccount,
  Connection,
  HostConnection,
  McpConnection,
  WalletConnection,
} from "../types.mjs";
import type { NamedTool } from "../../host/index.mjs";

export type Auth = string | Readonly<{
  url?: string | undefined;
  challenge?: string | undefined;
  verify?: string | undefined;
  logout?: string | undefined;
  /** URI resources echoed into the exact EIP-4361 message before signing. */
  resources?: readonly string[] | undefined;
  returnToken?: boolean | undefined;
}>;

export type AuthorizeAccessKey = Readonly<{
  address?: `0x${string}` | undefined;
  chainId?: bigint | undefined;
  expiry: number;
  keyType?: "secp256k1" | "p256" | "webAuthn" | undefined;
  limits?: readonly Readonly<{
    token: `0x${string}`;
    limit: bigint;
    period?: number | undefined;
  }>[] | undefined;
  /** Accounts policy for reusing a locally persisted matching signer. */
  reuse?: Readonly<{
    minExpiry?: number | undefined;
    minLimits?: readonly Readonly<{
      token: `0x${string}`;
      limit: bigint;
      period?: number | undefined;
    }>[] | undefined;
  }> | undefined;
  publicKey?: `0x${string}` | undefined;
  scopes?: readonly Readonly<{
    address: `0x${string}`;
    selector?: `0x${string}` | string | undefined;
    recipients?: readonly `0x${string}`[] | undefined;
  }>[] | undefined;
}>;

export type Capabilities = Readonly<{
  /** SIWE authentication folded into the same passkey ceremony. */
  auth?: Auth | undefined;
  /** Access key authorized by the same passkey ceremony. */
  authorizeAccessKey?: AuthorizeAccessKey | undefined;
  /** Cloud accounts authorized by the same SIWE/passkey message. */
  cloudAccounts?: CloudAccounts | undefined;
  /** App-visible agent output. Omitted fields use the documented defaults. */
  agent?: AgentVisibility | undefined;
}>;

export type CloudAccounts = Readonly<Partial<Record<CloudAccount, true>>>;

export type AgentVisibility = Readonly<{
  /** Expose completed assistant messages to the app. @default true */
  finalMessages?: boolean | undefined;
  /** Expose summaries of capabilities used during a turn. @default true */
  actionSummaries?: boolean | undefined;
  /** Allow the app to read retained conversation history. @default false */
  conversationHistory?: boolean | undefined;
  /** Expose the raw ordered agent event stream. Implies every other visibility. @default false */
  rawTraces?: boolean | undefined;
}>;

export declare namespace connect {
  type Options = Readonly<{
    capabilities?: Capabilities | undefined;
    /** Use account-hosted authorization with no access key, spending, or contract authority. */
    authorization?: "access_key" | "hosted" | undefined;
    /** Let an owning UI close the dialog after its connected state commits. @default "auto" */
    dialog?: Readonly<{ close?: "auto" | "manual" | undefined }> | undefined;
    /** Nanocodex permission preset selected by the app. @default "agent.run" */
    permission?: string | undefined;
    /**
     * Exact existing account-owned hosted MCP connections requested by this app.
     * Endpoints, provider tokens, and registration credentials are never accepted.
     */
    mcpConnections?: readonly McpConnection[] | undefined;
    /** Optional requested MCP connection to foreground in the hosted dialog. */
    focusMcpConnectionId?: string | undefined;
    /** Select one newly approved durable conversation by an app-generated UUIDv4. */
    conversationId?: string | undefined;
    /** Exact app-local tool catalog bound into the signed grant. */
    tools?: readonly NamedTool[] | undefined;
    signal?: AbortSignal | undefined;
  }>;
  type ReturnType = Promise<Connection>;
  type ErrorType = Error;
}

export function connect(client: Base<PrincipalInstance>, options: connect.Options): Promise<HostConnection>;
export function connect(client: Base<undefined>, options: connect.Options): Promise<WalletConnection>;
export function connect(client: Client, options: connect.Options): connect.ReturnType;

export declare namespace disconnect {
  type Options = Readonly<{ signal?: AbortSignal | undefined }>;
  type ReturnType = Promise<void>;
  type ErrorType = Error;
}

export function disconnect(client: Client, options?: disconnect.Options | undefined): disconnect.ReturnType;

export declare namespace reconnect {
  type Options = Readonly<{
    /** Reject a retained grant outside these app capability boundaries. */
    capabilities?: Pick<Capabilities, "agent" | "cloudAccounts"> | undefined;
    authorization?: "access_key" | "hosted" | undefined;
    tools?: readonly NamedTool[] | undefined;
    /** Reject a retained grant issued for another permission preset. */
    permission?: string | undefined;
    /** Reject a retained grant with a different exact hosted MCP slice. */
    mcpConnectionIds?: readonly string[] | undefined;
    /** Reject a retained grant for another durable conversation. */
    conversationId?: string | undefined;
    signal?: AbortSignal | undefined;
  }>;
  type ReturnType = Promise<Connection | undefined>;
  type ErrorType = Error;
}

/** Restores and validates this app's persisted grant session, if one exists. */
export function reconnect(client: Base<PrincipalInstance>, options?: reconnect.Options | undefined): Promise<HostConnection | undefined>;
export function reconnect(client: Base<undefined>, options?: reconnect.Options | undefined): Promise<WalletConnection | undefined>;
export function reconnect(client: Client, options?: reconnect.Options | undefined): reconnect.ReturnType;
