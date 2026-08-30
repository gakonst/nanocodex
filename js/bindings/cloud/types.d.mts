export type Hex = `0x${string}`;

export type CloudAccount = "github" | "gmail" | "gdrive" | "x" | "whoop" | "chatgpt";

export type AccessKey = Readonly<{
  address: Hex;
  chainId: bigint;
  keyId: Hex;
  /** Public key material is present only when the caller supplied an external key. */
  publicKey?: Hex | undefined;
  keyType: "secp256k1" | "p256" | "webAuthn";
  limits: readonly Readonly<{ token: Hex; limit: bigint; period?: number | undefined }>[];
  scopes: readonly Readonly<{
    address: Hex;
    selector?: Hex | string | undefined;
    recipients?: readonly Hex[] | undefined;
  }>[];
  witness: Hex;
  expiry: number;
  /** Canonical RLP-encoded, root-signed TIP-1053 authorization. */
  authorization?: Hex | undefined;
}>;

export type AgentTurnResult = Readonly<{
  turnId: string;
  finalMessage: string;
  provider: string;
  capabilitiesUsed: readonly string[];
  usage: import("../types.mjs").TurnUsage | null;
  usageError?: string | undefined;
  cursor?: string | undefined;
}>;

export type AgentVisibility = Readonly<{
  finalMessages: boolean;
  actionSummaries: boolean;
  conversationHistory: boolean;
  rawTraces: boolean;
}>;

/** Secret-free metadata for one account-owned hosted MCP connection. */
export type McpConnection = Readonly<{
  /** Exact opaque 43-character hosted connection ID. */
  id: string;
  /** Account-visible display name; endpoints and credentials remain broker-owned. */
  name: string;
}>;

export type AgentTurn = Readonly<{
  idempotencyKey: string;
  accepted(): Promise<string>;
  state(): Promise<import("../managed/Agent.mjs").TurnView>;
  steer(options: Readonly<{ input: import("../types.mjs").PromptInput }>): Promise<Readonly<{
    turn_id: string;
    state: "steering";
  }>>;
  result(options?: import("../managed/Agent.mjs").TurnResultOptions): Promise<AgentTurnResult>;
  cancel(): Promise<import("../managed/Agent.mjs").TurnView | Readonly<{
    turn_id: string;
    state: "cancelling";
  }>>;
}>;

export type ConnectAgent = Readonly<{
  id: string;
  /** Alias matching the canonical Nanocodex Agent surface. */
  sessionId: string;
  type: "connect";
  provider: string;
  events: Readonly<{
    page(options?: import("../managed/Agent.mjs").EventHistoryOptions): Promise<import("../managed/Agent.mjs").EventHistoryPage>;
    watch(options?: import("../managed/Agent.mjs").WatchEventsOptions): AsyncIterableIterator<import("../managed/Agent.mjs").Event>;
  }>;
  mercator: Readonly<{
    enabled: true;
    readonly channelId: Hex | undefined;
    readonly cumulative: bigint;
    readonly opened: boolean;
  }>;
  turn: Readonly<{
    prompt(options: import("../managed/Agent.mjs").PromptOptions): AgentTurn;
  }>;
  state(): Promise<import("../managed/Agent.mjs").State>;
  session: Readonly<{ shutdown(): Promise<void> }>;
}>;

export type Grant = Readonly<{
  id: Hex;
  permission: string;
  status: "active" | "revoked" | "expired";
  expiresAt: number;
  capabilities: readonly string[];
  visibility: AgentVisibility;
  /** Secret-free cloud account providers bound to this grant. */
  connectors: readonly CloudAccount[];
  /** Exact secret-free hosted MCP connections bound to this grant. */
  mcpConnections: readonly McpConnection[];
}>;

export type MppPermission = Readonly<{
  token: Hex;
  symbol: string;
  balance: bigint;
  balanceStatus: "pending" | "ready";
  settlementToken: Hex;
  settlementSymbol: string;
  settlementBalance: bigint;
  spent: bigint;
  limit: bigint;
  period: number;
  maxPerRequest: bigint;
}>;

export type Connection = Readonly<{
  accountAddress: Hex;
  agentId: string;
  grant: Grant;
  accessKey: AccessKey;
  mpp: MppPermission;
}>;

export type MachineUsdConfig = Readonly<{
  chainId: number;
  minUsdAmountCents: number;
  maxUsdAmountCents: number;
  onrampEnabled: boolean;
  stripePublishableKey: string;
  tokenAddress: Hex;
}>;

export type MachineUsdFunding = Readonly<{
  order: Readonly<{
    id: string;
    status: string;
    usdAmountCents: number;
    machineUsdAmount: bigint;
    issuanceTransactionHash: Hex;
  }>;
  connection: Connection;
}>;

export type MppCharge = Readonly<{
  receipt: Readonly<{
    id: string;
    amount: bigint;
    origin: string;
    transactionHash: Hex;
  }>;
  connection: Connection;
}>;
