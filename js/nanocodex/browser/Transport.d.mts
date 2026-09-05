import type {
  ChatGptSubscriptionHandle,
  MppSession,
} from "../types.mjs";
import type { Options as ManagedClientOptions } from "../managed/Agent.mjs";
import type {
  BrowserWebSocketConnection,
  BrowserWebSocketRequest,
} from "./host.mjs";

declare const responsesTransport: unique symbol;
declare const workerTransport: unique symbol;
declare const managedTransport: unique symbol;

export type ResponsesTransport = Readonly<{
  [responsesTransport]: true;
}>;
export type ManagedTransport = Readonly<{ [managedTransport]: true }>;
export type Transport = ResponsesTransport | ManagedTransport;

/** A Responses transport whose complete descriptor can cross a module Worker boundary. */
export type WorkerTransport = ResponsesTransport & Readonly<{
  [workerTransport]: true;
}>;

type SharedEndpointOptions = Readonly<{
  apiBaseUrl?: string | undefined;
  websocketUrl?: string | undefined;
  /** Open the persistent socket as soon as Agent.create returns. Defaults to true for hostManaged. */
  websocketPreconnect?: boolean | undefined;
  websocketWarmup?: boolean | undefined;
}>;

type WorkerEndpointOptions = SharedEndpointOptions & Readonly<{
  WebSocketImpl?: never;
  createWebSocket?: never;
}>;

type EndpointOptions = SharedEndpointOptions & Readonly<{
  WebSocketImpl?: typeof WebSocket | undefined;
  createWebSocket?(
    endpoint: string,
    sessionId: string,
    request: BrowserWebSocketRequest,
  ): WebSocket | BrowserWebSocketConnection | Promise<WebSocket | BrowserWebSocketConnection>;
}>;

export function openAi(options: WorkerEndpointOptions & Readonly<{
  apiKey: string;
}>): WorkerTransport;
export function openAi(options: EndpointOptions & Readonly<{
  apiKey: string;
}>): ResponsesTransport;

export function chatGpt(options: EndpointOptions & Readonly<{
  subscription: ChatGptSubscriptionHandle;
}>): ResponsesTransport;

/** Same-origin Nanocodex Responses proxy; defaults to `/api/responses`. */
export function hostManaged(options?: WorkerEndpointOptions): WorkerTransport;
export function hostManaged(options?: EndpointOptions): ResponsesTransport;

export function mpp(options: EndpointOptions & Readonly<{
  session: MppSession;
}>): ResponsesTransport;

export type ManagedIdentity =
  | Readonly<{ create: true; id?: never }>
  | Readonly<{ id: string; create?: never }>;

/** Account-authenticated durable Agent transport with explicit create/open identity. */
export function managed(options: ManagedClientOptions & Readonly<{
  agent: ManagedIdentity;
}>): ManagedTransport;
