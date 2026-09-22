import type { Principal } from "./account-auth";
import { PROBE_OWNER } from "./provider-probe-schedule";
type Config = { NANOCODEX_ADMIN_USER_ID?: string;
  NANOCODEX_PROVIDER_PROBE_COORDINATOR?: { getByName(name: string): { dashboardSnapshot(): Promise<unknown> } } };
/** Deployment-wide operational data is platform-admin only, never inferred from
 * an organization role, user API key, inference key or Connect grant. */
export async function routerDashboard(request: Request, env: Config, principal?: Principal): Promise<Response> {
  const json = (body: unknown, status = 200) => Response.json(body, { status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
  if (!principal) return json({ error: "unauthorized" }, 401);
  if (!env.NANOCODEX_ADMIN_USER_ID || principal.userId !== env.NANOCODEX_ADMIN_USER_ID
    || principal.kind !== "account_session" || principal.connectGrant) return json({ error: "forbidden" }, 403);
  if (request.method !== "GET") return json({ error: "method_not_allowed" }, 405);
  if (new URL(request.url).search) return json({ error: "invalid_request" }, 400);
  if (!env.NANOCODEX_PROVIDER_PROBE_COORDINATOR) return json({ error: "telemetry_unavailable" }, 503);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const snapshot = await Promise.race([
      env.NANOCODEX_PROVIDER_PROBE_COORDINATOR.getByName(PROBE_OWNER).dashboardSnapshot(),
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("telemetry deadline")), 2000); }),
    ]);
    return json(snapshot);
  } catch { return json({ error: "telemetry_unavailable" }, 503); }
  finally { clearTimeout(timer); }
}
