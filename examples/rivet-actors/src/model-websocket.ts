import WebSocket from "ws";
import type { BrowserWebSocketRequest } from "nanocodex/host";

import type { SubscriptionSnapshot } from "./auth.js";

const OPENAI_WEBSOCKET_BETA = "responses_websockets=2026-02-06";
const MAX_ERROR_BODY_BYTES = 4 * 1024;

export type SubscriptionProvider = {
  snapshot(): Promise<SubscriptionSnapshot>;
  recover(revision: number): Promise<SubscriptionSnapshot>;
};

type SocketDescriptor = {
  socket: globalThis.WebSocket;
  status: number;
  requestId?: string;
  serverModel?: string;
  reasoningIncluded: boolean;
  turnState?: string;
};

export async function openApiKeyWebSocket(
  endpoint: string,
  sessionId: string,
  request: BrowserWebSocketRequest,
): Promise<SocketDescriptor> {
  if (request.authorization !== "bearer" || !request.bearerToken) {
    throw new Error("the API-key WebSocket requires bearer authorization");
  }
  return upgrade(endpoint, sessionId, {
    bearerToken: request.bearerToken,
    ...(request.accountId === undefined ? {} : { accountId: request.accountId }),
    ...(request.fedramp === undefined ? {} : { fedramp: request.fedramp }),
    ...(request.turnState === undefined ? {} : { turnState: request.turnState }),
  });
}

export async function openSubscriptionWebSocket(
  auth: SubscriptionProvider,
  endpoint: string,
  sessionId: string,
  request: BrowserWebSocketRequest,
): Promise<SocketDescriptor> {
  if (request.authorization !== "host_managed" && request.authorization !== "preconnect") {
    throw new Error("the ChatGPT WebSocket requires host-managed authorization");
  }
  let snapshot = await auth.snapshot();
  try {
    return await upgrade(endpoint, sessionId, {
      ...snapshot,
      ...(request.turnState === undefined ? {} : { turnState: request.turnState }),
    });
  } catch (error) {
    if (!(error instanceof WebSocketUpgradeError) || error.status !== 401) throw error;
    snapshot = await auth.recover(snapshot.revision);
    return upgrade(endpoint, sessionId, {
      ...snapshot,
      ...(request.turnState === undefined ? {} : { turnState: request.turnState }),
    });
  }
}

class WebSocketUpgradeError extends Error {
  constructor(readonly status: number, detail: string) {
    super(`OpenAI WebSocket upgrade failed with HTTP ${status}${detail ? `: ${detail}` : ""}`);
  }
}

async function upgrade(
  endpoint: string,
  sessionId: string,
  request: {
    bearerToken: string;
    accountId?: string;
    fedramp?: boolean;
    turnState?: string;
  },
): Promise<SocketDescriptor> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${request.bearerToken}`,
    "OpenAI-Beta": OPENAI_WEBSOCKET_BETA,
    "User-Agent": "nanocodex-rivet-actors/0.1.0",
    "session-id": sessionId,
    "thread-id": sessionId,
    "x-client-request-id": sessionId,
    "x-openai-internal-codex-responses-lite": "true",
    "x-responsesapi-include-timing-metrics": "true",
  };
  if (request.accountId) headers["ChatGPT-Account-ID"] = request.accountId;
  if (request.fedramp) headers["X-OpenAI-Fedramp"] = "true";
  if (request.turnState) headers["x-codex-turn-state"] = request.turnState;

  return new Promise((resolve, reject) => {
    const socket = new WebSocket(endpoint, {
      handshakeTimeout: 15_000,
      headers,
      maxPayload: 32 * 1024 * 1024,
    });
    let responseHeaders: Record<string, string | string[] | undefined> = {};
    let settled = false;
    socket.once("upgrade", (response) => {
      responseHeaders = response.headers;
    });
    socket.once("open", () => {
      if (settled) return;
      settled = true;
      resolve({
        socket: socket as unknown as globalThis.WebSocket,
        status: 101,
        ...header(responseHeaders, "x-request-id", "requestId"),
        ...header(responseHeaders, "openai-model", "serverModel"),
        reasoningIncluded: responseHeaders["x-reasoning-included"] !== undefined,
        ...header(responseHeaders, "x-codex-turn-state", "turnState"),
      });
    });
    socket.once("unexpected-response", async (_request, response) => {
      if (settled) return;
      settled = true;
      const body = await readNodeBody(response, MAX_ERROR_BODY_BYTES).catch(() => "");
      socket.terminate();
      reject(new WebSocketUpgradeError(response.statusCode ?? 500, body));
    });
    socket.once("error", (error) => {
      if (settled) return;
      settled = true;
      reject(error);
    });
  });
}

function header<K extends string>(
  headers: Record<string, string | string[] | undefined>,
  name: string,
  key: K,
): { [P in K]?: string } {
  const value = headers[name];
  const normalized = Array.isArray(value) ? value[0] : value;
  return normalized === undefined ? {} : { [key]: normalized } as { [P in K]?: string };
}

async function readNodeBody(
  response: AsyncIterable<Buffer | string>,
  limit: number,
): Promise<string> {
  const chunks: Buffer[] = [];
  let bytes = 0;
  for await (const value of response) {
    const chunk = Buffer.isBuffer(value) ? value : Buffer.from(value);
    bytes += chunk.byteLength;
    if (bytes > limit) throw new Error(`response exceeded ${limit} bytes`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}
