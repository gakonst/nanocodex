import { requestOriginContext, requestOriginLocation, type RequestOriginContext } from "nanocodex/tools/environment";
import type { AccountMachine } from "./account-info";

export type CallerContext = Readonly<{
  reported?: RequestOriginContext;
  principal?: Readonly<{ kind: string; user_id: string }>;
}>;

/** The edge overwrites the principal assertion after authentication. Device context is only a claim. */
export function callerContext(headers: Headers): CallerContext {
  let reported: RequestOriginContext | undefined;
  let principal: CallerContext["principal"];
  const context = headers.get("x-nanocodex-client-context");
  try { if (context && context.length <= 2048) reported = requestOriginContext(JSON.parse(context)); } catch { /* Ignore invalid optional hints. */ }
  const asserted = headers.get("x-nanocodex-request-principal");
  try {
    const value = asserted && asserted.length <= 512 ? JSON.parse(asserted) : undefined;
    if (value && ["account_session", "api_key", "connect_grant", "service"].includes(value.kind)
      && typeof value.user_id === "string" && value.user_id.length <= 256) principal = { kind: value.kind, user_id: value.user_id };
  } catch { /* Internal callers predating attribution need no assertion. */ }
  return { ...(reported ? { reported } : {}), ...(principal ? { principal } : {}) };
}

export function projectCaller(context: CallerContext, hands: readonly AccountMachine[]) {
  const claimed = context.reported;
  const location = requestOriginLocation(claimed?.location);
  const hand = claimed?.hand ? hands.find(hand => hand.id === claimed.hand) : undefined;
  const originRoot = hand && claimed?.cwd && [hand.mount, ...(hand.aliases ?? [])]
    .find(root => claimed.cwd === root || claimed.cwd!.startsWith(`${root}/`));
  const cwd = originRoot && hand && claimed?.cwd ? hand.mount + claimed.cwd.slice(originRoot.length) : null;
  return {
    client: claimed?.client ? { name: claimed.client, attribution: "client_reported" as const } : null,
    hand: hand ? { key: hand.id, path: hand.mount, attribution: "client_reported_authorized_hand" as const } : null,
    ...(location ? { location: { ...location, attribution: "client_reported" as const } } : {}),
    ...(context.principal ? { principal: context.principal } : {}),
    ...(claimed ? { cwd, timezone: claimed.timezone ?? null } : {}),
  };
}
