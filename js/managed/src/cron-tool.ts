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
        input: { type: "string", minLength: 1, description: "Prompt to run on every occurrence, at most 64 KiB. Include all context needed by a fresh session." },
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
