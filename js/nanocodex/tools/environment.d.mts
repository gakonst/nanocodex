export type EnvironmentHand = Readonly<{
  name: string; path: string; capabilities: readonly string[];
  kind?: string; online?: boolean; provider?: string; vm_provider?: string;
}>;
export type EnvironmentConnection = Readonly<{
  id: string; label: string; accountId?: string; capabilities?: readonly string[];
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
  }>[];
  apis: readonly unknown[]; identity: Readonly<Record<string, unknown>>;
  stablecoins: readonly unknown[]; authorizations: readonly unknown[]; vault: readonly unknown[];
}>;
export type AgentEnvironment = Readonly<{
  runtime: string; default_cwd: string; status: string;
  hands: Readonly<Record<string, EnvironmentHand>>;
  accounts: Readonly<Record<string, EnvironmentAccount>>;
  apis: readonly unknown[]; identity: Readonly<Record<string, unknown>>;
  stablecoins: readonly unknown[]; authorizations: readonly unknown[]; vault: readonly unknown[];
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
