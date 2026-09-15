import type { UserDataScope } from "./user-data-scope";
import {
  authenticate,
  requireSameOriginMutation,
  type AccountAuthEnv,
  type Principal,
} from "./account-auth";
import {
  UserDataError,
  isUserDataMutation,
  parseUserDataOperation,
} from "nanocodex-tools/user-data";

const USER_ASSERTION = "x-nanocodex-user-id";

export type UserDataRouteEnv = AccountAuthEnv & {
  NANOCODEX_USER_DATA: DurableObjectNamespace<UserDataScope>;
};

type AuthenticateUserDataRequest = (
  request: Request,
  env: AccountAuthEnv,
  url: URL,
) => Promise<Principal | undefined>;

/** Public HTTP boundary for the same per-user operation contract used by the agent tool. */
export async function routeUserDataRequest(
  request: Request,
  env: UserDataRouteEnv,
  url = new URL(request.url),
  authenticateRequest: AuthenticateUserDataRequest = authenticate,
): Promise<Response | undefined> {
  if (url.pathname !== "/v1/data") return undefined;
  if (request.method !== "POST") return json({ error: "method_not_allowed" }, 405);
  if (url.search !== "") return json({ error: "invalid_request" }, 400);
  const principal = await authenticateRequest(request, env, url);
  if (!principal) return json({ error: "unauthorized" }, 401);
  try {
    const operation = parseUserDataOperation(await request.json());
    const capability = isUserDataMutation(operation) ? "data:write" : "data:read";
    if (!principal.capabilities.includes(capability)) {
      return json({ error: "forbidden", message: `request lacks ${capability} capability` }, 403);
    }
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
    const data = env.NANOCODEX_USER_DATA.getByName(principal.userId);
    const initialized = await data.fetch("https://user-data.internal/initialize", {
      method: "PUT",
      headers: { [USER_ASSERTION]: principal.userId },
    });
    if (!initialized.ok) return initialized;
    return data.fetch("https://user-data.internal/operations", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        [USER_ASSERTION]: principal.userId,
      },
      body: JSON.stringify(operation),
    });
  } catch (error) {
    if (error instanceof UserDataError) {
      return json({ error: error.code, message: error.message }, 400);
    }
    return json({ error: "invalid_json", message: "request body must be JSON" }, 400);
  }
}

function json(body: unknown, status: number): Response {
  return Response.json(body, { status, headers: { "cache-control": "no-store" } });
}
