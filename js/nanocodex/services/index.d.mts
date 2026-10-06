import type { AccountServices } from "./account.mjs";
export type { AccountServices, AccountReceipt, AccountInput, ConnectorProvider, OAuthProvider, ConnectorCapability, ConnectorStatus, ConnectorCatalog, AccountLinks, AccountLinksQuery, ConnectorStart, VaultInputs, SshInput, CapturedStore, CardInput } from "./account.mjs";
/** Exact account-owned resources authorized by a signed Connect grant. */
export type ServiceCapabilities = Readonly<{
  vault?: Readonly<{ ids: readonly string[]; origins: readonly string[]; request: boolean }>;
  phone?: Readonly<{ numberIds: readonly string[]; read: boolean; provision: boolean; release: boolean }>;
}>;
export function normalizeServices(value: ServiceCapabilities): ServiceCapabilities;
export function normalizeServices(value: undefined): undefined;
export function serviceResource(value: ServiceCapabilities): string;
export type RequestOptions = Readonly<{ signal?: AbortSignal | undefined }>;
export type VaultMetadata = Readonly<{ id: string; name: string; created_at: number }> & (
  | Readonly<{ kind: 'api_key' }>
  | Readonly<{ kind: 'login'; username: string; browser_origin?: string }>
  | Readonly<{ kind: 'card'; last4: string }>
  | Readonly<{ kind: 'phone'; phone_number: string }>
  | Readonly<{ kind: 'address'; address_line_1: string; address_line_2?: string; city: string; state: string; zip: string; country: string }>
  | Readonly<{ kind: 'totp'; issuer: string; account: string; origin: string; algorithm: 'SHA1' | 'SHA256' | 'SHA512'; digits: 6 | 8; period: number }>
);
export type VaultRequest = Readonly<{
  vault_id: string;
  url: string;
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'HEAD' | 'OPTIONS';
  headers?: Readonly<Record<string, string>>;
  body?: string;
  body_encoding?: 'raw' | 'json' | 'form';
  signing?: Readonly<{
    algorithm: 'HMAC-SHA256' | 'HMAC-SHA512' | 'RS256' | 'ES256' | 'EdDSA';
    message?: string;
    jwt?: Readonly<{ header: Readonly<Record<string, unknown>>; payload: Readonly<Record<string, unknown>> }>;
    encoding?: 'hex' | 'base64' | 'base64url';
    key_encoding?: 'utf8' | 'base64' | 'hex' | 'pkcs8';
  }>;
}>;
export type BrokerReceipt = Readonly<{ status: number; ok: boolean }>;
export type ConnectServiceTransport = Readonly<{
  appOrigin?: string | undefined;
  fetch(input: string | URL | Request, init?: RequestInit): Promise<Response>;
}>;
export type ServicesOptions = Readonly<{ baseUrl?: string; fetch?: typeof globalThis.fetch }> & (
  | Readonly<{ apiKey: string; connect?: never; grantId?: never }>
  | Readonly<{ connect: ConnectServiceTransport; grantId: string; apiKey?: never }>
);
export class ServiceError extends Error {
  constructor(message: string, options?: { status?: number; code?: string; outcomeUnknown?: boolean });
  readonly status: number | undefined;
  readonly code: string | undefined;
  readonly outcomeUnknown: boolean;
}
export type HostedRequest = Readonly<{ url: string; origin: string; appOrigin?: string; state: string }> & (Readonly<{ service: 'vault'; action: 'enroll' | 'select'; kind: VaultMetadata['kind'] }> | Readonly<{ service: 'phone'; operationId: string }>);
export type VaultSelectionResult = Readonly<{ type: 'nanocodex:service-enrollment'; service: 'vault'; action: 'select'; state: string; vault_id: string; kind: VaultMetadata['kind']; name: string }>;
export type VaultHostedResult = Readonly<{ type: 'nanocodex:service-enrollment'; service: 'vault'; action?: 'enroll'; state: string; vault_id: string; kind: VaultMetadata['kind']; name: string; origin?: string }>;
export type PhoneHostedResult = Readonly<{ type: 'nanocodex:service-enrollment'; service: 'phone'; state: string; operation_id: string; status: 'complete' | 'denied' | 'expired' | 'failed' }>;
export type HostedResult = VaultHostedResult | VaultSelectionResult | PhoneHostedResult;
export function createHostedRequest(options: Readonly<{ host?: string; appOrigin?: string; state?: string }> & (Readonly<{ service?: 'vault'; action?: 'enroll' | 'select'; kind?: VaultMetadata['kind'] }> | Readonly<{ service: 'phone'; operationId: string }>)): HostedRequest;
export function readHostedResult(event: MessageEvent, request: HostedRequest, source: MessageEventSource | null): HostedResult | undefined;
export function openHostedPopup(request: HostedRequest, options?: RequestOptions & Readonly<{ window?: Window }>): Promise<HostedResult>;
export type PhoneNumber = Readonly<{ id: string; phone_number: string; country: 'US'; status: 'active' | 'release_pending' | 'released'; created_at: string }>;
export type AvailablePhoneNumber = Readonly<{ phone_number: string; country: 'US'; type: 'local' }>;
export type PhoneQuote = Readonly<{ id: string; currency: string; monthly_price: string; inbound_sms_price: string; recurring: true; expires_at: string }>;
export type PhoneRequest = Readonly<{
  /** Caller UUID used for polling and identical intent reconciliation. */
  operation_id: string;
  /** Owner-side UUID used only for the hosted account approval. */
  approval_request_id?: string;
  kind: 'purchase' | 'release'; status: 'pending_approval' | 'complete' | 'denied' | 'expired' | 'failed' | 'outcome_unknown';
  phone_number: string; number_id?: string; quote?: PhoneQuote; created_at: string; error?: string;
}>;
export type PhoneMessage = Readonly<{ id: string; from: string; to: string; body: string; received_at: string; expires_at: string }>;
export type PhoneProvision = Readonly<{ operation_id: string; phone_number: string; country: 'US' }>;
export type PhoneRelease = Readonly<{ operation_id: string }>;
export type PhoneMessagesQuery = Readonly<{ cursor?: string; limit?: number }>;
export type HostedLinkQuery = Readonly<{service?:'vault'|'phone';action?:'enroll'|'select';kind?:VaultMetadata['kind'];operation_id?:string;app_origin?:string;state?:string}>;
export type ServicesClient = Readonly<{
  account: AccountServices;
  links: AccountServices['links'];
  hosted(query?: HostedLinkQuery, options?: RequestOptions): Promise<HostedRequest>;
  catalog(options?: RequestOptions): Promise<Readonly<{ services: readonly Readonly<{ id: string; path?: string; [key: string]: unknown }>[] }>>;
  vault: Readonly<{
    list(options?: RequestOptions): Promise<Readonly<{ vault: readonly VaultMetadata[] }>>;
    get(id: string, options?: RequestOptions): Promise<Readonly<{ entry: VaultMetadata }>>;
    request(input: VaultRequest, options?: RequestOptions): Promise<BrokerReceipt>;
  }>;
  phone: Readonly<{
    available(query?: Readonly<{ country?: 'US'; area_code?: string; limit?: number }>, options?: RequestOptions): Promise<Readonly<{ numbers: readonly AvailablePhoneNumber[] }>>;
    list(options?: RequestOptions): Promise<Readonly<{ numbers: readonly PhoneNumber[] }>>;
    provision(input: PhoneProvision, options?: RequestOptions): Promise<Readonly<{ request: PhoneRequest }>>;
    get(id: string, options?: RequestOptions): Promise<Readonly<{ number: PhoneNumber }>>;
    release(id: string, input: PhoneRelease, options?: RequestOptions): Promise<Readonly<{ request: PhoneRequest }>>;
    messages(id: string, query?: PhoneMessagesQuery, options?: RequestOptions): Promise<Readonly<{ messages: readonly PhoneMessage[]; next_cursor: string | null }>>;
    requests: Readonly<{ get(id: string, options?: RequestOptions): Promise<Readonly<{ request: PhoneRequest }>> }>;
  }>;
}>;
/** Keep direct account keys server-side. Browser apps should use a scoped Connect client. */
export function createServicesClient(options: ServicesOptions): ServicesClient;
