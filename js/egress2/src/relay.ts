/** Existing private Linux egress container supplies the subscription's outbound IP. */
export function relayChatGpt(request: Request, ownerId: string, namespace: DurableObjectNamespace): Promise<Response> {
  const target = new URL(request.url);
  const stub = namespace.get(namespace.idFromName(`user-v1:${ownerId}`));
  return stub.fetch(new Request(`https://chatgpt-egress.internal${target.pathname}${target.search}`, request));
}

/** The VPC binding reaches the subscription upstream without the account relay DO. */
export function routeChatGpt(
  request: Request,
  ownerId: string,
  bindings: { GATEWAY?: Fetcher; CHATGPT_EGRESS?: DurableObjectNamespace },
): Promise<Response> {
  if (bindings.GATEWAY) return bindings.GATEWAY.fetch(request);
  if (bindings.CHATGPT_EGRESS) return relayChatGpt(request, ownerId, bindings.CHATGPT_EGRESS);
  throw new Error("ChatGPT outbound route is unavailable");
}
