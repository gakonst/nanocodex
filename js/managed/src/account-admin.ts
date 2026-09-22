import { accountCommunication } from "./account-communication";
import type { Principal } from "./account-auth";

type AdminConfig = Parameters<typeof accountCommunication>[0] & { NANOCODEX_ADMIN_USER_ID?: string };

/** Operator-assigned access; organization ownership never implies platform administration. */
export async function accountAdmin(request: Request, config: AdminConfig, principal: Principal | undefined): Promise<Response> {
  const json = (body: unknown, status = 200) => Response.json(body, {
    status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
  if (!principal) return json({ error: "unauthorized" }, 401);
  if (!config.NANOCODEX_ADMIN_USER_ID || principal.userId !== config.NANOCODEX_ADMIN_USER_ID
    || principal.kind !== "account_session" || principal.connectGrant) return json({ error: "forbidden" }, 403);
  if (request.method !== "GET") return json({ error: "method_not_allowed" }, 405);
  if (new URL(request.url).search) return json({ error: "invalid_request" }, 400);
  const emailAssigned = principal.userId === config.NANOCODEX_EMAIL_OWNER_ID;
  const phoneAssigned = principal.userId === config.NANOCODEX_PHONE_OWNER_ID;
  let contacts;
  let emailAvailable = false;
  try {
    contacts = await accountCommunication(config, principal.userId);
    emailAvailable = contacts.email !== null;
  } catch {
    contacts = await accountCommunication({ ...config, NANOCODEX_EMAIL: undefined }, principal.userId);
  }
  return json({
    email: { assigned: emailAssigned, configured: Boolean(config.NANOCODEX_EMAIL), available: emailAvailable, address: contacts.email },
    phone: { assigned: phoneAssigned, configured: /^\+[1-9]\d{1,14}$/.test(config.TWILIO_VOICE_FROM_NUMBER ?? ""), address: contacts.phone },
  });
}
