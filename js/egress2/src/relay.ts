import { tracing, setSpanAttributes } from "nanocodex/cloudflare/tracing";

type Relays = { GATEWAY?: Fetcher; CHATGPT_EGRESS?: DurableObjectNamespace };

export function relayChatGpt(request: Request, ownerId: string, bindings: Relays): Promise<Response> {
  const target = new URL(request.url);
  const namespace = bindings.CHATGPT_EGRESS;
  if (!namespace) throw new Error("ChatGPT outbound route is unavailable");
  const stub = namespace.getByName(`user-v1:${ownerId}`);
  return tracing.enterSpan("egress2.relay", async span => {
    // Egress2 overwrites this private ID before dispatch; it joins this span to
    // responses_egress and the account Container DO without logging owner data.
    const requestId = request.headers.get("x-nanocodex-egress-request-id");
    setSpanAttributes(span, {
      "egress2.request_id": requestId && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(requestId)
        ? requestId : undefined });
    const response = await stub.fetch(new Request(`https://chatgpt-egress.internal${target.pathname}${target.search}`, request));
    span.setAttribute("http.response.status_code", response.status);
    return response;
  });
}

/** The VPC binding reaches the subscription upstream without the account relay DO. */
export function routeChatGpt(request: Request, ownerId: string, bindings: Relays): Promise<Response> {
  if (bindings.GATEWAY) {
    const headers = new Headers(request.headers);
    headers.delete("x-nanocodex-egress-request-id"); // correlation stays on the private Container DO hop
    const gateway = bindings.GATEWAY;
    return tracing.enterSpan("egress2.gateway", async span => {
      const response = await gateway.fetch(new Request(request, { headers }));
      setSpanAttributes(span, { "egress2.transport": "gateway", "http.response.status_code": response.status });
      return response;
    });
  }
  return relayChatGpt(request, ownerId, bindings);
}
