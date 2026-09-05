import {
  createResponsesTransport,
  nonEmpty,
} from "../runtime/responses-transport.mjs";
import { createManagedTransport } from "../runtime/managed-transport.mjs";

export function openAi(options) {
  const apiKey = nonEmpty(options?.apiKey, "OpenAI API key");
  return createResponsesTransport({
    apiKey,
    ...endpoints(options),
  });
}

export function chatGpt(options) {
  if (!options?.subscription || typeof options.subscription !== "object") {
    throw new TypeError("ChatGPT transport requires a subscription handle");
  }
  return createResponsesTransport({
    subscription: options.subscription,
    ...endpoints(options),
  });
}

export function mpp(options) {
  if (!options?.session || typeof options.session.ws !== "function") {
    throw new TypeError("MPP transport requires a session with ws(endpoint)");
  }
  return createResponsesTransport({
    mpp: options.session,
    ...endpoints(options),
  });
}

/** Account-authenticated durable Agent transport with explicit create/open identity. */
export function managed(options) {
  return createManagedTransport(options);
}

function endpoints(options = {}) {
  return {
    apiBaseUrl: options.apiBaseUrl,
    websocketUrl: options.websocketUrl,
    websocketWarmup: options.websocketWarmup,
  };
}
