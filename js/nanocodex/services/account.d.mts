import type { RequestOptions, VaultMetadata } from './index.mjs';
export type ConnectorProvider = 'google' | 'github' | 'slack' | 'x' | 'spotify' | 'soundcloud' | 'cloudflare' | 'link' | 'whatsapp';
export type OAuthProvider = Exclude<ConnectorProvider, 'cloudflare' | 'whatsapp'>;
export type ConnectorCapability = ConnectorProvider | 'gmail' | 'gdrive' | 'gcalendar' | 'gtasks' | 'gdocs' | 'gsheets' | 'gslides' | 'gcontacts';
export type AccountInput = Readonly<Record<string, unknown>>;
/** Safe server metadata; private model-login receipts belong only in a trusted client UI. */
export type AccountReceipt = Readonly<{
  id?: string; status?: string; phase?: string; state?: string; ok?: boolean; ready?: boolean;
  configured?: boolean; connected?: boolean; saved?: boolean; deleted?: boolean;
  accountId?: string; account_id?: string; connection_id?: string; vault_id?: string; capture_id?: string;
  provider?: string; kind?: string; name?: string; label?: string; email?: string; last4?: string;
  authorization_url?: string; verification_url?: string; verification_uri?: string;
  verification_uri_complete?: string; user_code?: string; expires_at?: number | string; poll_after_ms?: number;
  login?: AccountReceipt; attempt?: string | AccountReceipt; capabilities?: readonly string[];
  connections?: readonly AccountReceipt[]; mcp_connections?: readonly AccountReceipt[]; mcp_connection?: AccountReceipt;
  models?: readonly (string | AccountReceipt)[]; scopes?: readonly string[];
  balance?: number; currency?: string; freshness?: string; observed_at?: number; provider_updated_at?: number; retry_after?: number;
  [key: string]: unknown;
}>;
export type ConnectorStatus = Readonly<{ connected: boolean; account?: string; connections?: readonly Readonly<{ id: string; label: string; accountId?: string; capabilities?: readonly string[]; scopes?: readonly string[] }>[] }>;
export type ConnectorCatalog = Readonly<{ providers: readonly Readonly<{ id: ConnectorProvider; name: string; description: string; capabilities: readonly Readonly<{ id: ConnectorCapability; name: string }>[] }>[] }>;
export type AccountLinks = Readonly<{ connections: string; vault: string; wallet: string; access: string }>;
export type AccountLinksQuery = Readonly<{ connect?: ConnectorProvider | 'claude' | 'chatgpt' | 'openai' | 'mcp'; add?: VaultMetadata['kind'] }>;
export type ConnectorStart = Readonly<{ return_to?: string }>;
export type VaultInputs = {
  login: Readonly<{ name: string; username: string; password: string; browser_origin?: string }>;
  api_key: Readonly<{ name: string; api_key: string }>;
  card: Readonly<{ name: string; card_number: string; expiry_month: string; expiry_year: string; cvv?: string; billing_zip: string }>;
  address: Readonly<{ name: string; address_line_1: string; address_line_2?: string; city: string; state: string; zip: string; country: string }>;
  phone: Readonly<{ name: string; phone_number: string }>;
  totp: Readonly<{ name: string; origin: string }> & (Readonly<{ otpauth_uri: string }> | Readonly<{ seed: string; issuer: string; account: string; algorithm?: 'SHA1' | 'SHA256' | 'SHA512'; digits?: 6 | 8; period?: number }>);
};
/** Host authority of a Vault SSH target: a pinned fingerprint, an owner-saved
 * device host-key trust binding (`device` = this reference's server Hand,
 * `hand:MACHINE_ID` = an exact enrolled Hand), or both. */
export type SshHostKeyTrust = 'device' | `hand:${string}`;
export type SshHostAuthority =
  | Readonly<{ host_key_sha256: string; host_key_trust?: SshHostKeyTrust }>
  | Readonly<{ host_key_sha256?: never; host_key_trust: SshHostKeyTrust }>;
export type SshInput = Readonly<{ hostname: string; port: number; username: string }> & SshHostAuthority & (Readonly<{ generate: true; private_key?: never }> | Readonly<{ private_key: string; generate?: never }>);
export type CapturedStore = Readonly<{ capture_id: string; operation_id: string; name?: string; address_vault_id?: string }>;
export type CardInput = (Readonly<{ vault_id: string; capture_id?: never }> | Readonly<{ capture_id: string; vault_id?: never }>) & (Readonly<{ operation: 'status' | 'balance'; operation_id?: string }> | Readonly<{ operation: 'refresh'; operation_id: string }>);
type Read = (options?: RequestOptions) => Promise<AccountReceipt | undefined>;
type Login = Readonly<{
  start(input?: Readonly<Record<string, never>>, options?: RequestOptions): Promise<AccountReceipt>;
  status(options?: RequestOptions): Promise<AccountReceipt>;
  disconnect: Read;
}>;
export type AccountServices = Readonly<{
  links(query?: AccountLinksQuery, options?: RequestOptions): Promise<AccountLinks>;
  connectors: Readonly<{
    catalog(options?: RequestOptions): Promise<ConnectorCatalog>;
    list(options?: RequestOptions): Promise<Readonly<{ connectors: Partial<Record<ConnectorCapability, ConnectorStatus>> }>>;
    start(provider: OAuthProvider, input?: ConnectorStart, options?: RequestOptions): Promise<AccountReceipt>;
    disconnect(provider: ConnectorProvider, id: string, options?: RequestOptions): Promise<AccountReceipt | undefined>;
    cloudflare(input: Readonly<{ vault_id: string; account_id?: string }>, options?: RequestOptions): Promise<AccountReceipt>;
    link: Readonly<{ status(attempt: string, options?: RequestOptions): Promise<AccountReceipt> }>;
    whatsapp: Readonly<{ start(input: Readonly<{ phone: string; operation_id: string }>, options?: RequestOptions): Promise<AccountReceipt>; status(options?: RequestOptions): Promise<AccountReceipt> }>;
    mcp: Readonly<{
      list(options?: RequestOptions): Promise<Readonly<{ mcp_connections: readonly AccountReceipt[] }>>;
      create(input: Readonly<{ target: string }>, options?: RequestOptions): Promise<AccountReceipt>;
      start(id: string, input?: ConnectorStart, options?: RequestOptions): Promise<AccountReceipt>;
      disconnect(id: string, options?: RequestOptions): Promise<AccountReceipt | undefined>;
    }>;
  }>;
  credentials: Readonly<{
    overview(options?: RequestOptions): Promise<AccountReceipt>;
    chatgpt: Login;
    claude: Login & Readonly<{ complete(input: Readonly<{ code: string }>, options?: RequestOptions): Promise<AccountReceipt> }>;
    openai: Readonly<{ save(input: Readonly<{ api_key: string }>, options?: RequestOptions): Promise<AccountReceipt | undefined>; delete: Read }>;
  }>;
  vault: Readonly<{
    list(options?: RequestOptions): Promise<Readonly<{ vault: readonly VaultMetadata[] }>>;
    get(id: string, options?: RequestOptions): Promise<Readonly<{ entry: VaultMetadata }>>;
    create<K extends keyof VaultInputs>(kind: K, input: VaultInputs[K], options?: RequestOptions): Promise<VaultMetadata>;
    delete(kind: VaultMetadata['kind'], id: string, options?: RequestOptions): Promise<AccountReceipt | undefined>;
    loginOrigin(id: string, input: Readonly<{ browser_origin: string }>, options?: RequestOptions): Promise<VaultMetadata>;
    ssh: Readonly<{ put(reference: string, input: SshInput, options?: RequestOptions): Promise<AccountReceipt>; remove(reference: string, options?: RequestOptions): Promise<AccountReceipt | undefined> }>;
    store(input: CapturedStore, options?: RequestOptions): Promise<AccountReceipt>;
    card(input: CardInput, options?: RequestOptions): Promise<AccountReceipt>;
  }>;
}>;
export function createAccountServices(request: (path: string, method: string, body: AccountInput | undefined, options?: RequestOptions) => Promise<unknown>): AccountServices;
