import type { NamedTool, ToolContext } from "nanocodex";
import type { Principal } from "./account-auth";

/** Dispatch through the public router with a trusted principal, never HTTP identity headers. */
export function workspacePushTools(options: {
  sessionId: string;
  ownerId: string;
  authorizationEpoch: number;
  origin: string;
  authorization(context: ToolContext): Principal | undefined;
  request(request: Request, principal: Principal): Promise<Response>;
}): NamedTool[] {
  return (["gmail", "calendar"] as const).map(service => ({
    name: `${service}_watch`,
    description: service === "gmail"
      ? "Enable continuous Gmail notifications for this agent, inspect status, or disable an exact connected Google account's watch. Enable requires its mailbox email. Optional crm=true explicitly imports relevant correspondence into the private CRM. Select the exact connection_id from connected accounts. Does not send email. Direct account authorization required; unavailable through Connect."
      : "Enable continuous Calendar meeting import into the private CRM, inspect status, or disable a watch for this agent and exact Google connection/calendar. Enable requires explicit crm=true; calendar_id defaults to primary. This does not supply meeting notes or send invitations. Direct account authorization required; unavailable through Connect.",
    parameters: { type: "object", additionalProperties: false, required: ["operation", "connection_id"], properties: {
      operation: {type: "string", enum: ["enable", "status", "disable"]},
      connection_id: {type: "string", pattern: "^[A-Za-z0-9_-]{43}$"},
      ...(service === "gmail" ? { email: {type: "string", maxLength: 320, description: "Required for enable: connected mailbox email."}, crm: {type: "boolean", description: "Optional explicit CRM opt-in on enable."} }
        : { calendar_id: {type: "string", minLength: 1, maxLength: 1024}, crm: {type: "boolean", enum: [true], description: "Required true for enable."} }),
    } },
    handler: async (input: unknown, context: ToolContext) => {
      context.signal.throwIfAborted();
      const principal = options.authorization(context);
      const body = input as Record<string, unknown>;
      const capability = body?.operation === "status" ? "agents:read" : "agents:write";
      if (!principal || (principal.kind !== "account_session" && principal.kind !== "api_key")
        || principal.connectGrant !== undefined || principal.userId !== options.ownerId
        || principal.authorizationEpoch !== options.authorizationEpoch
        || !principal.capabilities.includes(capability) || !principal.capabilities.includes("tools:use")) {
        throw new Error(`Watch requires current direct account authorization with ${capability} and tools:use`);
      }
      if (!body || typeof body !== "object" || Array.isArray(body)
        || !["enable", "status", "disable"].includes(body.operation as string)
        || typeof body.connection_id !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(body.connection_id)) throw new TypeError("Invalid watch operation or connection_id");
      const enable = body.operation === "enable";
      const allowed = ["operation", "connection_id", ...(service === "calendar" ? ["calendar_id"] : []), ...(enable ? service === "gmail" ? ["email", "crm"] : ["crm"] : [])];
      if (Object.keys(body).some(key => !allowed.includes(key))) throw new TypeError("Unexpected watch argument");
      if (service === "gmail" && enable && (typeof body.email !== "string" || body.email.length > 320 || !/^[^\s@]+@[^\s@]+$/.test(body.email)
        || (body.crm !== undefined && typeof body.crm !== "boolean"))) throw new TypeError("Enable requires a mailbox email and optional boolean crm");
      if (service === "calendar" && ((enable && body.crm !== true) || (body.calendar_id !== undefined
        && (typeof body.calendar_id !== "string" || !body.calendar_id.trim() || body.calendar_id.length > 1024 || /[\u0000-\u001f\u007f]/.test(body.calendar_id))))) throw new TypeError("Calendar enable requires crm=true and a valid calendar_id");
      const url = new URL(`/v1/agents/${encodeURIComponent(options.sessionId)}/${service}-push/${body.connection_id}`, options.origin);
      if (service === "calendar") url.searchParams.set("calendar_id", (body.calendar_id as string | undefined) ?? "primary");
      const headers = new Headers({origin: url.origin});
      if (enable) headers.set("content-type", "application/json");
      const payload = service === "calendar" ? {crm: true} : {email: body.email, ...(body.crm === undefined ? {} : {crm: body.crm})};
      const response = await options.request(new Request(url, {method: enable ? "PUT" : body.operation === "status" ? "GET" : "DELETE", headers,
        ...(enable ? {body: JSON.stringify(payload)} : {}), signal: context.signal}), principal);
      if (!response.ok) { await response.body?.cancel(); throw new Error(`${service} watch request failed (HTTP ${response.status})`); }
      return response.json();
    },
  }));
}
