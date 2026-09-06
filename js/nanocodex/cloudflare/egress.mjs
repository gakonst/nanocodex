import { cloudflareEgressSubject } from "./egress-subject.mjs";

const OPENAI_WEBSOCKET_BETA = "responses_websockets=2026-02-06";
const BROKER_API_BASE_URL = "https://nanocodex.internal/v1";
const BROKER_WEBSOCKET_URL = `${BROKER_API_BASE_URL}/responses`;
const OPTION_NAMES = new Set(["binding"]);

/**
 * Builds the function-backed endpoint options consumed by Transport.hostManaged.
 * Provider credentials are deliberately not part of this boundary.
 */
export function cloudflareEgress(options) {
  const { binding } = validateOptions(options);
  return Object.freeze({
    apiBaseUrl: BROKER_API_BASE_URL,
    websocketUrl: BROKER_WEBSOCKET_URL,
    historyNotes: Object.freeze({
      async available() {
        const headers = brokerHistoryHeaders(binding);
        const response = await binding.fetch("https://nanocodex.internal/.well-known/nanocodex/context-management", {
          method: "GET", headers, signal: AbortSignal.timeout(10_000),
        });
        if (!response.ok) { await response.body?.cancel(); return false; }
        return (await response.json())?.enabled === true;
      },
      request({ path, body, budget, threadId, signal }) {
        // The private broker independently enforces the closed operation allowlist.
        if (!/^alpha\/(?:history|notes)\/v2\/[a-z_]+$/.test(path)) {
          throw new Error("Invalid history/notes operation");
        }
        const headers = brokerHistoryHeaders(binding);
        headers.set("content-type", "application/json");
        headers.set("session-id", body.context.session_id);
        headers.set("thread-id", threadId);
        headers.set("x-openai-tool-output-truncation-policy", JSON.stringify(budget));
        return binding.fetch(`${BROKER_API_BASE_URL}/${path}`, {
          method: "POST", headers, body: JSON.stringify(body), signal,
        });
      },
    }),
    createWebSocket: (endpoint, sessionId, request) =>
      openBrokeredWebSocket(binding, endpoint, sessionId, request),
  });
}

function brokerHistoryHeaders(binding) {
  const headers = new Headers({ Authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL" });
  const subject = cloudflareEgressSubject(binding);
  if (subject !== undefined) headers.set("x-nanocodex-subject", subject);
  return headers;
}

function validateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Cloudflare EGRESS requires options");
  }
  const unexpected = Object.keys(options).find((name) => !OPTION_NAMES.has(name));
  if (unexpected) {
    throw new TypeError(
      `Cloudflare EGRESS does not accept ${unexpected}; provider credentials belong in the private broker`,
    );
  }
  if (!options.binding || typeof options.binding.fetch !== "function") {
    throw new TypeError("Cloudflare EGRESS binding must provide fetch(input, init)");
  }
  return options;
}

async function openBrokeredWebSocket(
  binding,
  endpoint,
  sessionId,
  request,
) {
  if (request?.authorization !== "host_managed" && request?.authorization !== "preconnect") {
    throw new Error("Cloudflare EGRESS requires Transport.hostManaged authorization");
  }
  if (typeof sessionId !== "string" || !sessionId) {
    throw new TypeError("Cloudflare EGRESS requires a non-empty session ID");
  }
  const threadId = request?.threadId ?? sessionId;
  if (typeof threadId !== "string" || !threadId) {
    throw new TypeError("Cloudflare EGRESS requires a non-empty thread ID");
  }
  const url = exactWebSocketEndpoint(endpoint, BROKER_WEBSOCKET_URL);
  url.protocol = "https:";
  const headers = new Headers({
    Authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
    Upgrade: "websocket",
    "OpenAI-Beta": OPENAI_WEBSOCKET_BETA,
    "session-id": sessionId,
    "thread-id": threadId,
    "x-client-request-id": threadId,
    "x-openai-internal-codex-responses-lite": "true",
    "x-responsesapi-include-timing-metrics": "true",
    "User-Agent": "nanocodex-js/cloudflare",
  });
  const subject = cloudflareEgressSubject(binding);
  if (subject !== undefined) headers.set("x-nanocodex-subject", subject);
  if (typeof request.turnState === "string" && request.turnState) {
    headers.set("x-codex-turn-state", request.turnState);
  }

  const response = await binding.fetch(url, { method: "GET", headers });
  const socket = response?.webSocket;
  if (response?.status !== 101 || !socket || typeof socket.accept !== "function") {
    if (socket && typeof socket.close === "function") socket.close();
    await response?.body?.cancel?.();
    throw brokerRejection(response);
  }
  socket.binaryType = "arraybuffer";
  socket.accept();
  return {
    socket,
    status: response.status,
    requestId: response.headers?.get("x-request-id") ?? undefined,
    serverModel: response.headers?.get("openai-model") ?? undefined,
    reasoningIncluded: response.headers?.has("x-reasoning-included") ?? false,
    turnState: response.headers?.get("x-codex-turn-state") ?? undefined,
  };
}

function exactWebSocketEndpoint(endpoint, expected) {
  let url;
  try {
    url = new URL(endpoint);
  } catch {
    throw new TypeError("Cloudflare EGRESS received an invalid Responses WebSocket endpoint");
  }
  if (url.href !== expected) {
    throw new Error("Cloudflare EGRESS denied an unexpected Responses WebSocket endpoint");
  }
  return url;
}

function brokerRejection(response) {
  const status = Number.isInteger(response?.status) ? response.status : 502;
  const retryAfterHeader = response?.headers?.get("retry-after") ?? null;
  const retryAfter = Number(retryAfterHeader);
  return Object.assign(
    new Error(`Cloudflare EGRESS broker rejected the Responses WebSocket with HTTP ${status}`),
    {
      status,
      body: "credential_broker_rejected",
      ...(retryAfterHeader !== null && Number.isFinite(retryAfter) && retryAfter >= 0
        ? { retryAfter }
        : {}),
    },
  );
}
