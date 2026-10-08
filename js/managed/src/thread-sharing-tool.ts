import type { NamedTool, ToolContext } from "nanocodex";
import type { Principal } from "./account-auth";

const SESSION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const LINK_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** Dispatch through the owner-authenticated public router, never model-supplied identity headers. */
export function threadSharingTools(options: {
  sessionId: string;
  ownerId: string;
  authorizationEpoch: number;
  origin: string;
  authorization(context: ToolContext): Principal | undefined;
  request(request: Request, principal: Principal): Promise<Response>;
}): NamedTool[] {
  return [{
    name: "thread_sharing",
    description: "List, create, or revoke this thread's share links. session_id defaults to the current thread; another thread must belong to the same owner and account scope. List returns active link metadata, never bearer URLs. Create requires explicit user authorization to share the full conversation, including tool results; permission defaults to read. Only explicitly requested write access permits guests to submit turns. Anyone holding a created URL has its permission: do not send it to others without authorization. Revoke requires a listed link_id; revoke_all disables every active link and closes live shared streams. Revocation does not cancel turns already admitted. Never automatically retry create after an uncertain result: list links and resolve the outcome first. Direct account root agent only; unavailable to Connect grants and shared guests.",
    parameters: { type: "object", additionalProperties: false, required: ["operation"], properties: {
      operation: { type: "string", enum: ["list", "create", "revoke", "revoke_all"] },
      session_id: { type: "string", pattern: SESSION_ID.source, description: "Thread ID; defaults to this thread." },
      permission: { type: "string", enum: ["read", "write"], description: "Create only. Defaults to read; write requires explicit user authorization." },
      link_id: { type: "string", pattern: LINK_ID.source, description: "Required for revoke only. Use an ID returned by list." },
    } },
    handler: async (input: unknown, context: ToolContext) => {
      context.signal.throwIfAborted();
      const principal = options.authorization(context);
      if (context.subagent !== undefined || !principal
        || (principal.kind !== "account_session" && principal.kind !== "api_key")
        || principal.connectGrant !== undefined || principal.userId !== options.ownerId
        || principal.authorizationEpoch !== options.authorizationEpoch
        || !principal.capabilities.includes("agents:read") || !principal.capabilities.includes("tools:use")) {
        throw new Error("Thread sharing requires current direct account root authorization with agents:read and tools:use");
      }
      if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("Expected a thread sharing operation");
      const body = input as Record<string, unknown>;
      if (!["list", "create", "revoke", "revoke_all"].includes(body.operation as string)) throw new TypeError("Invalid thread sharing operation");
      const allowed = ["operation", "session_id", ...(body.operation === "create" ? ["permission"] : body.operation === "revoke" ? ["link_id"] : [])];
      if (Object.keys(body).some(key => !allowed.includes(key))
        || (body.session_id !== undefined && (typeof body.session_id !== "string" || !SESSION_ID.test(body.session_id)))
        || (body.permission !== undefined && !["read", "write"].includes(body.permission as string))
        || (body.operation === "revoke" && (typeof body.link_id !== "string" || !LINK_ID.test(body.link_id)))) {
        throw new TypeError("Invalid or unexpected thread sharing argument");
      }
      if (body.operation !== "list" && !principal.capabilities.includes("agents:write")) throw new Error("Changing thread sharing requires agents:write");
      const sessionId = (body.session_id as string | undefined) ?? options.sessionId;
      const url = new URL(`/v1/agents/${sessionId}/share-links${body.operation === "revoke" ? `/${body.link_id}` : ""}`, options.origin);
      const create = body.operation === "create";
      let response: Response;
      try {
        response = await options.request(new Request(url, {
          method: body.operation === "list" ? "GET" : create ? "POST" : "DELETE",
          headers: { origin: url.origin, ...(create ? { "content-type": "application/json" } : {}) },
          ...(create ? { body: JSON.stringify({ permission: body.permission ?? "read" }) } : {}),
          signal: context.signal,
        }), principal);
      } catch (error) {
        if (create) throw new Error("Share link creation outcome is unknown; do not retry automatically. List active links first.", { cause: error });
        throw error;
      }
      if (!response.ok) {
        await response.body?.cancel();
        if (create && response.status >= 500) throw new Error(`Share link creation outcome is unknown (HTTP ${response.status}); do not retry automatically. List active links first.`);
        throw new Error(`Thread sharing ${body.operation} failed (HTTP ${response.status})`);
      }
      if (body.operation === "revoke") return { session_id: sessionId, id: body.link_id, revoked: true };
      // Do not turn a late cancellation into a false failure after a confirmed mutation.
      try { return { session_id: sessionId, ...await response.json<Record<string, unknown>>() }; }
      catch (error) {
        if (create) throw new Error("Share link creation succeeded but its URL could not be read; do not retry automatically. List active links first.", { cause: error });
        throw error;
      }
    },
  }];
}

/** Shared transcripts must not redistribute link authority (including Code Mode output). */
export function redactSharedLinkTokens<T>(value: T): T {
  if (typeof value === "string") return value.replace(/nsl_[A-Za-z0-9_-]{43}/g, "[redacted share token]") as T;
  if (Array.isArray(value)) return value.map(item => redactSharedLinkTokens(item)) as T;
  if (value !== null && typeof value === "object") return Object.fromEntries(
    Object.entries(value).map(([key, item]) => [redactSharedLinkTokens(key), redactSharedLinkTokens(item)]),
  ) as T;
  return value;
}

/** Emit ordinary text immediately, including final n/ns/nsl suffixes. Once a
 * bearer prefix completes, mask its underscore and following 43 characters.
 * The already visible "nsl" cannot grant access. Replay must prime this state. */
export function sharedTextStream() {
  let tail = "";
  let remaining = 0;
  return (delta: string): string => {
    const output: string[] = [];
    for (const character of delta) {
      if (remaining > 0) {
        if (/^[A-Za-z0-9_-]$/.test(character)) { remaining--; continue; }
        remaining = 0;
      }
      if (tail + character === "nsl_") {
        output.push("[redacted share token]");
        remaining = 43;
        tail = "";
      } else {
        output.push(character);
        tail = (tail + character).slice(-3);
      }
    }
    return output.join("");
  };
}
