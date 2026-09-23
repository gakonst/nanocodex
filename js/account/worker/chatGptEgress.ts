import { Container } from "@cloudflare/containers";

export class ChatGptEgress extends Container {
  defaultPort = 8080;
  enableInternet = true;
  sleepAfter = "1h";

  /** Private egress binding: transfer the small SDP exchange in one RPC reply. */
  async createRealtimeCall(body: string, headers: Record<string, string>, search: string): Promise<{
    status: number; headers: Record<string, string>; body: string;
  }> {
    const target = new URL("https://chatgpt-egress.internal/backend-api/codex/realtime/calls");
    target.search = search;
    const response = await this.fetch(new Request(target, {
      method: "POST", headers, body,
    }));
    const began = performance.now();
    const answer = await response.text();
    const sessionId = headers["x-session-id"];
    console.info({ type: "voice.relay.body", transport: "rpc", duration_ms: performance.now() - began,
      ...(sessionId && /^[0-9a-f-]{36}$/.test(sessionId) ? { voice_session_id: sessionId } : {}) });
    return { status: response.status, headers: Object.fromEntries(response.headers), body: answer };
  }

  override async fetch(request: Request): Promise<Response> {
    const pathname = new URL(request.url).pathname;
    if (pathname === "/backend-api/codex/responses"
      && request.headers.get("upgrade")?.toLowerCase() === "websocket") {
      const began = performance.now();
      const wasRunning = this.ctx.container?.running;
      // Generated here, never copied from a caller's identifier or credential.
      // The Node relay does not forward this private correlation header upstream.
      const relayId = crypto.randomUUID();
      // Parent ID is generated/overwritten by egress at the private DO boundary.
      // UUID validation bounds the log value; it does not authenticate its origin.
      const egressRequestId = request.headers.get("x-nanocodex-egress-request-id");
      const headers = new Headers(request.headers);
      headers.set("x-nanocodex-relay-id", relayId);
      headers.delete("x-nanocodex-egress-request-id");
      let status: number | undefined;
      try {
        const response = await super.fetch(new Request(request, { headers }));
        status = response.status;
        // Preserve the exact upgrade Response and its webSocket; do not wrap it.
        return response;
      } finally {
        try {
          console.info({
            type: "responses.relay", transport: "websocket", relay_id: relayId,
            ...(egressRequestId
              && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(egressRequestId)
              ? { egress_request_id: egressRequestId } : {}),
            was_running: wasRunning, duration_ms: performance.now() - began,
            ...(status === undefined ? { outcome: "error" } : { status, outcome: "response" }),
          });
        } catch { /* Observability must not change the upgrade result. */ }
      }
    }
    if (pathname !== "/backend-api/codex/realtime/calls") {
      return super.fetch(request);
    }
    const began = performance.now();
    const wasRunning = this.ctx.container?.running;
    const response = await super.fetch(request);
    let timing: Record<string, unknown> = {};
    try { timing = JSON.parse(response.headers.get("x-nanocodex-relay-timing") ?? "{}"); } catch { /* Older relay image. */ }
    const sessionId = request.headers.get("x-session-id");
    console.info({
      type: "voice.relay",
      ...(sessionId && /^[0-9a-f-]{36}$/.test(sessionId) ? { voice_session_id: sessionId } : {}),
      was_running: wasRunning,
      duration_ms: performance.now() - began,
      status: response.status,
      ...Object.fromEntries(["process_age_ms", "fetch_ms", "socket_wait_ms", "upload_ms", "response_wait_ms"]
        .flatMap((key) => typeof timing?.[key] === "number" && Number.isFinite(timing[key]) && timing[key] >= 0
          ? [[key, timing[key]]] : [])),
      ...(typeof timing?.socket_reused === "boolean" ? { socket_reused: timing.socket_reused } : {}),
    });
    const headers = new Headers(response.headers);
    headers.delete("x-nanocodex-relay-timing");
    return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
  }
}

// Separate classes create independent container applications with regional
// constraints. They share the relay image/behavior; no credential state moves.
export class ChatGptEgressWnam extends ChatGptEgress {}
export class ChatGptEgressEnam extends ChatGptEgress {}
export class ChatGptEgressWeur extends ChatGptEgress {}
export class ChatGptEgressEeur extends ChatGptEgress {}
export class ChatGptEgressApac extends ChatGptEgress {}
export class ChatGptEgressSam extends ChatGptEgress {}
export class ChatGptEgressOc extends ChatGptEgress {}
