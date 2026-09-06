import {
  createResponsesTransport,
  nonEmpty,
} from "../runtime/responses-transport.mjs";
import { createManagedTransport } from "../runtime/managed-transport.mjs";
import { defaultHostManagedWebSocketUrl } from "./hostManagedWebSocket.mjs";

export function openAi(options) {
  const apiKey = nonEmpty(options?.apiKey, "OpenAI API key");
  return createResponsesTransport({
    apiKey,
    ...connection(options),
  });
}

export function chatGpt(options) {
  if (!options?.subscription || typeof options.subscription !== "object") {
    throw new TypeError("ChatGPT transport requires a subscription handle");
  }
  return createResponsesTransport({
    subscription: options.subscription,
    ...connection(options),
  });
}

export function hostManaged(options = {}) {
  const websocketUrl = options.websocketUrl
    ?? (options.createWebSocket ? undefined : defaultHostManagedWebSocketUrl());
  return createResponsesTransport({
    hostAuth: true,
    hostManagedProtocol: true,
    ...connection({ ...options, websocketUrl }),
    websocketPreconnect: options.websocketPreconnect ?? true,
  });
}

export function mpp(options) {
  if (!options?.session || typeof options.session.ws !== "function") {
    throw new TypeError("MPP transport requires a session with ws(endpoint)");
  }
  return createResponsesTransport({
    mpp: options.session,
    ...connection(options),
  });
}

/** Account-authenticated durable Agent transport with explicit create/open identity. */
export function managed(options) {
  return createManagedTransport(options);
}

function connection(options = {}) {
  return {
    WebSocketImpl: options.WebSocketImpl,
    apiBaseUrl: options.apiBaseUrl,
    createWebSocket: options.createWebSocket,
    websocketUrl: options.websocketUrl,
    websocketPreconnect: options.websocketPreconnect,
    websocketWarmup: options.websocketWarmup,
  };
}
