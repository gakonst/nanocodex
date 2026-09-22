import type { NamedTool, ToolContext } from "nanocodex";
import { CRON_TRIGGER_ID, parseCronTrigger, type CronTriggerConfig, type cronTriggerView } from "./cron-triggers";

export function createCronTool(
  create: (id: string, config: CronTriggerConfig, context: ToolContext) => Promise<ReturnType<typeof cronTriggerView>>,
): NamedTool {
  return {
    name: "create_cron",
    description: [
      "Create a durable recurring prompt for this managed agent. It runs even when the user disconnects.",
      "Choose a stable id; repeating the same request is idempotent. An existing id with different settings is rejected.",
      "Schedules default to enabled, UTC, and a fresh session per occurrence; use session_mode continue to reuse this conversation.",
      "Available only with account authorization, never through a Connect grant.",
    ].join(" "),
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", pattern: CRON_TRIGGER_ID.source, description: "Stable schedule identifier, 1-64 letters, digits, underscores, or hyphens." },
        cron: { type: "string", maxLength: 256, description: "Five-field cron expression: minute hour day-of-month month day-of-week." },
        timezone: { type: "string", maxLength: 128, description: "IANA time zone, such as Europe/Athens. Defaults to UTC." },
        input: { type: "string", minLength: 1, description: "Prompt to run on every occurrence. Include all context needed by a fresh session." },
        enabled: { type: "boolean", default: true },
        session_mode: { type: "string", enum: ["new", "continue"], default: "new", description: "new starts a fresh session with this agent's settings; continue reuses this conversation and skips ticks while busy." },
      },
      required: ["id", "cron", "input"],
      additionalProperties: false,
    },
    handler: (input, context) => {
      if (!input || typeof input !== "object" || Array.isArray(input)) {
        throw new TypeError("create_cron input must be an object");
      }
      const { id, ...config } = input as Record<string, unknown>;
      if (typeof id !== "string" || !CRON_TRIGGER_ID.test(id)) {
        throw new TypeError("invalid cron trigger id");
      }
      return create(id, parseCronTrigger(config, Date.now()), context);
    },
  };
}

export type CronManagementInput = { agent_id?: string; id?: string; cron?: string; timezone?: string; input?: string; enabled?: boolean; session_mode?: "new" | "continue" };

export function cronManagementTools(
  run: (operation: "list" | "update" | "delete", input: CronManagementInput, context: ToolContext) => Promise<unknown>,
): NamedTool[] {
  return (["list", "update", "delete"] as const).map(operation => ({
    name: operation === "list" ? "list_crons" : `${operation}_cron`,
    description: `${operation === "list" ? "List account schedules, including disabled schedules. Omit agent_id to discover schedules across the account." : operation === "update" ? "Update an existing schedule. Omitted settings are preserved; enabled=false pauses it." : "Delete a schedule. Already accepted occurrences may still finish."} ${operation === "list" ? "" : "Use agent_id and id from list_crons; agent_id defaults to this agent."} Requires account authorization; unavailable through Connect grants.`,
    parameters: {
      type: "object",
      properties: {
        agent_id: { type: "string", minLength: 1, maxLength: 256, description: "Schedule owner agent ID returned by list_crons." },
        ...(operation === "list" ? {} : { id: { type: "string", pattern: CRON_TRIGGER_ID.source } }),
        ...(operation === "update" ? {
          cron: { type: "string", maxLength: 256 }, timezone: { type: "string", maxLength: 128 },
          input: { type: "string", minLength: 1 }, enabled: { type: "boolean" },
          session_mode: { type: "string", enum: ["new", "continue"] },
        } : {}),
      },
      required: operation === "list" ? [] : ["id"], additionalProperties: false,
    },
    handler: (value: unknown, context: ToolContext) => {
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("cron tool input must be an object");
      const input = value as CronManagementInput;
      const allowed = ["agent_id", ...(operation === "list" ? [] : ["id"]), ...(operation === "update" ? ["cron", "timezone", "input", "enabled", "session_mode"] : [])];
      if (Object.keys(input).some(key => !allowed.includes(key))
        || (input.agent_id !== undefined && (typeof input.agent_id !== "string" || !input.agent_id.length || input.agent_id.length > 256))
        || (operation !== "list" && (typeof input.id !== "string" || !CRON_TRIGGER_ID.test(input.id)))) throw new TypeError("invalid cron tool input");
      if (operation === "update") {
        const { agent_id, id, ...patch } = input;
        if (!Object.keys(patch).length) throw new TypeError("update_cron requires at least one setting");
        parseCronTrigger({ cron: "0 9 * * *", input: "validation", ...patch }, Date.now());
      }
      return run(operation, input, context);
    },
  }));
}
