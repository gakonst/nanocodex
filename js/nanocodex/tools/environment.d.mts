import type { HandResourceObservation } from "nanocodex-tools/internal/hosted-machine";

/** Public account metadata; a funded wallet does not imply payment authorization. */
export type EnvironmentWallet = Readonly<
  | { status: "disabled" | "not_configured" | "unavailable" }
  | { status: "ready"; address: string; created_at: number; chain: "tempo"; chain_id: 4217;
      balance: Readonly<{ status: "unavailable" } | { status: "ready"; amount: string; decimals: 6; symbol: "MACH"; token: string }> }
>;
export type EnvironmentHand = Readonly<{
  name: string; path: string; capabilities: readonly string[];
  resources: EnvironmentHandResources;
  kind?: string; online?: boolean; provider?: string; vm_provider?: string;
}>;
export type EnvironmentConnection = Readonly<{
  id: string; label: string; accountId?: string; capabilities?: readonly string[];
  scopes?: readonly string[];
}>;
export type EnvironmentAccount = Readonly<{
  connections: readonly EnvironmentConnection[]; label?: string;
  tool?: string; description?: string; documentation?: string;
}>;
export type AccountEnvironmentSource = Readonly<{
  status: string; authenticated?: readonly string[];
  accounts?: Readonly<Record<string, string>>;
  connectorAccounts?: Readonly<Record<string, readonly EnvironmentConnection[]>>;
  connectorTools?: Readonly<Record<string, Readonly<{ tool: string; description: string; documentation: string }>>>;
  machines?: readonly Readonly<{
    id: string; name: string; mount: string; capabilities: readonly string[];
    kind?: string; online?: boolean; provider?: string; vm_provider?: string;
    resources?: HandResourceObservation;
  }>[];
  apis: readonly unknown[]; identity: Readonly<Record<string, unknown>>;
  stablecoins: readonly unknown[]; authorizations: readonly unknown[]; vault: readonly unknown[];
  wallet?: EnvironmentWallet;
}>;
export type AgentEnvironment = Readonly<{
  runtime: string; default_cwd: string; status: string;
  hands: Readonly<Record<string, EnvironmentHand>>;
  accounts: Readonly<Record<string, EnvironmentAccount>>;
  apis: readonly unknown[]; identity: Readonly<Record<string, unknown>>;
  stablecoins: readonly unknown[]; authorizations: readonly unknown[]; vault: readonly unknown[];
  wallet?: EnvironmentWallet;
}>;
export function projectEnvironment(info: AccountEnvironmentSource, host: Readonly<{ runtime: string; default_cwd: string }>): AgentEnvironment;
export function contextData(tag: string, value: unknown): string;
/** Client-reported sensor sample. timestamp_ms is Unix milliseconds; accuracy_meters is horizontal accuracy. */
export type RequestOriginLocation = Readonly<{
  latitude: number; longitude: number; accuracy_meters: number; timestamp_ms: number; approximate: boolean;
}>;
export type RequestOriginContext = Readonly<{ client?: string; hand?: string; cwd?: string; timezone?: string; location?: RequestOriginLocation }>;
/** Drops invalid optional location samples or samples older than five minutes / over 30 seconds in the future. */
export function requestOriginLocation(value: unknown, now?: number): RequestOriginLocation | undefined;
export function requestOriginContext(value: unknown, now?: number): RequestOriginContext;

export type EnvironmentHandResources = Readonly<{ status: "unknown" } | (HandResourceObservation & { status: "fresh" | "stale" })>;
/** Samples older than five minutes or from offline hands are stale; missing/future samples are unknown. */
export function projectHandResources(value: unknown, online?: boolean, now?: number): EnvironmentHandResources;
