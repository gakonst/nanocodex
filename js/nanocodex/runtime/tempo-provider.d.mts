import type { McpPayment, McpServers, MppSession } from "../types.mjs";

export declare const DEFAULT_MERCATOR_MCP_URL = "https://mercator.tempo.xyz/mcp";

export type TempoProvider<Session extends MppSession = MppSession> = MppSession & Readonly<{
  kind: "tempo";
  /** The underlying session, available for channel and payment telemetry. */
  session: Session;
  /** MPP-aware HTTP fetch using the same methods as paid MCP calls. */
  fetch?: TempoPaymentFetch | undefined;
}>;

export function createTempoProvider<Session extends MppSession>(
  options: { session: Session; payment: McpPayment; fetch?: TempoPaymentFetch | undefined },
): TempoProvider<Session>;

export type TempoPaymentFetch = (
  input: RequestInfo | URL,
  init?: RequestInit | undefined,
  policy?: Readonly<{
    maxAmount?: bigint | undefined;
    intent?: "charge" | "session" | undefined;
  }> | undefined,
) => Promise<Response>;

type WalletParameters = {
  getClient: (...args: any[]) => any;
  resolveAccount: (...args: any[]) => any;
};

/** The adapter-neutral surface implemented by every Accounts SDK provider. */
export type AccountsWallet = {
  getMppxParameters(options?: { accessKey?: `0x${string}` | undefined }): WalletParameters;
  getAccount?(options?: unknown): { address: `0x${string}` };
  store?: {
    getState?(): { activeAccount?: number; accounts?: readonly { address: `0x${string}` }[]; chainId?: number };
    accessKeys?: {
      get(options: {
        account: `0x${string}`;
        accessKey: `0x${string}`;
        chainId: number;
      }): Promise<unknown>;
    };
  };
};

export type AccountsTempoPolicy = {
  /**
   * These values stay structurally typed so a wallet may use any compatible
   * Accounts/MPPx/Viem dependency instance without nominal package coupling.
   */
  autoSwap?: unknown;
  channelStore?: unknown;
  decimals?: number | undefined;
  escrow?: `0x${string}` | undefined;
  maxDeposit?: string | undefined;
  topUpAmount?: string | undefined;
  [option: string]: unknown;
};

export type AccountsTempoSessionOptions = AccountsTempoPolicy & {
  bootstrap?: boolean | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  webSocket?: unknown;
};

export type AccountsTempoMercatorOptions = AccountsTempoPolicy & {
  onChannelUpdate?: ((entry: {
    channelId: `0x${string}`;
    cumulativeAmount: bigint;
    [field: string]: unknown;
  }) => void | Promise<void>) | undefined;
};

export type AccountsTempoPaymentOptions = {
  /** Raw fetch wrapped by MPPx. */
  fetch?: typeof globalThis.fetch | undefined;
  /** Maximum atomic amount accepted from one HTTP or MCP challenge. */
  maxAmount?: bigint | undefined;
  /** Optional application approval composed after the amount check. */
  onPaymentRequired?: ((challenge: unknown) => boolean | Promise<boolean>) | undefined;
};

export type AccountsTempoSession = MppSession & {
  readonly channelId: `0x${string}` | undefined;
  readonly cumulative: bigint;
  readonly opened: boolean;
  readonly state: unknown;
  close(): Promise<unknown>;
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  topUp(amount: string | bigint): Promise<unknown>;
};

export type AccountsTempoProviderOptions = {
  /** Any provider returned by Accounts SDK `Provider.create(...)`. */
  wallet: AccountsWallet;
  /** Connected root account owning the delegated key. Inferred from Accounts when omitted. */
  account?: `0x${string}` | undefined;
  /** Optional Accounts SDK access key to pin for both model and MCP payments. */
  accessKey?: `0x${string}` | undefined;
  /** Chain on which the delegated key is authorized. Inferred from Accounts when omitted. */
  chainId?: number | undefined;
  /** Shared Tempo payment policy applied to the model session and Mercator. */
  policy?: AccountsTempoPolicy | undefined;
  /** Model-session-only overrides, such as `bootstrap` or `webSocket`. */
  session?: AccountsTempoSessionOptions | undefined;
  /** Mercator-only Tempo method overrides, such as `onChannelUpdate`. */
  mercator?: AccountsTempoMercatorOptions | undefined;
  /** Shared HTTP and MCP challenge policy. */
  payment?: AccountsTempoPaymentOptions | undefined;
};

/**
 * Constructs a Tempo provider from any Accounts SDK wallet adapter without
 * taking a runtime dependency on `accounts`.
 */
export function createTempoProviderFromAccounts(
  options: AccountsTempoProviderOptions,
): Promise<TempoProvider<AccountsTempoSession>>;

/** @internal */
export function resolveMcpServers(
  provider: MppSession | undefined,
  configured: McpServers | false | undefined,
): McpServers | undefined;
