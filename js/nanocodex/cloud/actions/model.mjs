import { createResponsesTransport } from "../../runtime/responses-transport.mjs";

const MODEL_PROTOCOL = "nanocodex-connect-v1";
const MODEL_TICKET_PROTOCOL_PREFIX = "nanocodex-ticket.";

/** Creates a local browser/WASM Responses transport backed by this Connect grant. */
export function transport(client, options) {
  const connection = options?.connection;
  if (!connection || typeof connection !== "object") {
    throw new TypeError("model.transport requires an active connection");
  }
  if (connection.grant?.status !== "active") {
    throw new Error("The Connect authorization is not active.");
  }
  if (!connection.grant.connectors?.includes("chatgpt")) {
    throw new Error("Connect ChatGPT before opening a local Nanocodex model transport.");
  }
  const grantSession = client._captureSession?.();
  if (!grantSession) {
    throw new Error("The Connect authorization session is unavailable.");
  }
  const grantId = connection.grant.id;
  const apiOrigin = new URL(client.transport.baseUrl).origin;

  return createResponsesTransport({
    async createWebSocket(_endpoint, sessionId, request) {
      const ticketResponse = await grantSession.request({
        method: "POST",
        path: `/v1/grants/${grantId}/model/ticket`,
        body: {
          session_id: sessionId,
          ...(request.turnState ? { turn_state: request.turnState } : {}),
        },
      });
      const ticket = ticketResponse?.ticket;
      if (typeof ticket !== "string" || ticket.length === 0) {
        throw new Error("Nanocodex Connect returned no model ticket.");
      }
      const url = new URL(`/v1/grants/${grantId}/model`, apiOrigin);
      url.searchParams.set("app_id", client.appId);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      url.searchParams.set("session_id", sessionId);
      const WebSocketImpl = globalThis.WebSocket;
      if (typeof WebSocketImpl !== "function") {
        throw new Error("WebSocket is unavailable in this runtime.");
      }
      return new WebSocketImpl(url, [MODEL_PROTOCOL, `${MODEL_TICKET_PROTOCOL_PREFIX}${ticket}`]);
    },
  });
}
