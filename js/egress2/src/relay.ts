import { tracing } from "cloudflare:workers";

const REGIONS = ["wnam", "enam", "weur", "eeur", "apac", "sam", "oc"] as const;
type Region = typeof REGIONS[number];

type Relays = { GATEWAY?: Fetcher; CHATGPT_EGRESS?: DurableObjectNamespace } &
  Partial<Record<`CHATGPT_EGRESS_${Uppercase<Region>}`, DurableObjectNamespace>>;

/** New names avoid existing legacy anchors; hints affect only the first lookup. */
export function relayChatGpt(request: Request, ownerId: string, bindings: Relays, requestedRegion?: string | null): Promise<Response> {
  const target = new URL(request.url);
  const region = REGIONS.find(value => value === requestedRegion);
  if (requestedRegion != null && !region) throw new Error("Invalid relay region");
  const namespace = region ? bindings[`CHATGPT_EGRESS_${region.toUpperCase() as Uppercase<Region>}`] : bindings.CHATGPT_EGRESS;
  if (!namespace) throw new Error("ChatGPT outbound route is unavailable");
  const name = region ? `text-v2:${region}:${ownerId}` : `user-v1:${ownerId}`;
  const stub = namespace.get(namespace.idFromName(name), region ? { locationHint: region } : undefined);
  return tracing.enterSpan("egress2.relay", async span => {
    span.setAttribute("egress2.relay.region", region ?? "legacy");
    // Egress2 overwrites this private ID before dispatch; it joins this span to
    // responses_egress and the account Container DO without logging owner data.
    const requestId = request.headers.get("x-nanocodex-egress-request-id");
    if (requestId && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(requestId)) {
      span.setAttribute("egress2.request_id", requestId);
    }
    const response = await stub.fetch(new Request(`https://chatgpt-egress.internal${target.pathname}${target.search}`, request));
    span.setAttribute("http.response.status_code", response.status);
    return response;
  });
}

/** The VPC binding reaches the subscription upstream without the account relay DO. */
export function routeChatGpt(request: Request, ownerId: string, bindings: Relays, region?: string | null): Promise<Response> {
  if (bindings.GATEWAY) {
    const headers = new Headers(request.headers);
    headers.delete("x-nanocodex-egress-request-id"); // correlation stays on the private Container DO hop
    return bindings.GATEWAY.fetch(new Request(request, { headers }));
  }
  return relayChatGpt(request, ownerId, bindings, region);
}
