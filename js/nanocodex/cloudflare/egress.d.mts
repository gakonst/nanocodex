import type {
  BrowserHttpRequest,
  BrowserWebSocketConnection,
  BrowserWebSocketRequest,
} from "../browser/host.mjs";

/** Structural subset of a private Cloudflare Service Binding. */
export type CloudflareEgressBinding = Readonly<{
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}>;

/** Plain private discovery results. Consumers must validate and project current authority. */
export type CloudflareAccountCatalogResult = Readonly<{ status: number; catalog: unknown }>;
export type CloudflareAccountVaultResult = Readonly<{ status: number; vault: unknown }>;

/** Metadata only; this surface never returns provider credentials or Vault secrets. */
export type CloudflareAccountMetadataBinding = CloudflareEgressBinding & Readonly<{
  readAccountCatalog?: (userId: string) => Promise<CloudflareAccountCatalogResult>;
  readAccountVault?: (userId: string) => Promise<CloudflareAccountVaultResult>;
}>;

export type CloudflareEgressOptions = Readonly<{
  /** The managed Worker's private EGRESS Service Binding. */
  binding: CloudflareEgressBinding;
  /** Provider credentials are accepted only by the separately deployed broker. */
  apiKey?: never;
  accessToken?: never;
  token?: never;
  /** Broker subjects are derived privately from the owning Durable Object. */
  subject?: never;
}>;

/** Exact function-backed options for `Transport.hostManaged(...)`. */
export type CloudflareEgressTransportOptions = Readonly<{
  apiBaseUrl: string;
  websocketUrl: string;
  createResponse(endpoint: string, sessionId: string, request: BrowserHttpRequest): Promise<Response>;
  createWebSocket(
    endpoint: string,
    sessionId: string,
    request: BrowserWebSocketRequest,
  ): Promise<BrowserWebSocketConnection>;
}>;

/**
 * Creates the brokered WebSocket and streaming HTTPS seams for a managed Cloudflare Worker.
 *
 * The managed Worker supplies only fixed placeholders to `binding`; the
 * separately deployed broker owns and injects the real provider credential.
 */
export declare function cloudflareEgress(
  options: CloudflareEgressOptions,
): CloudflareEgressTransportOptions;
