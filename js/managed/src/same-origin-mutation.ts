import type { Principal } from "./account-auth";

/** Runtime-neutral origin guard, usable by broker projections outside workerd. */
export function requireSameOriginMutation(
  request: Request,
  url: URL,
  principal: Principal,
): Response | undefined {
  if (principal.kind !== "account_session") return undefined;
  return request.headers.get("origin") === url.origin
    ? undefined
    : Response.json({ error: "forbidden_origin" }, {
      status: 403,
      headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
    });
}
