import type { AgentEvent, PromptInput, TurnUsage } from "nanocodex";
import type { HistoryCitation } from "./history-search";
import type { ManagedAgentSettings } from "./agent-settings";

export type ClientCommand =
  | { type: "prompt"; id: string; input: PromptInput }
  | { type: "steer"; id: string; input: PromptInput }
  | { type: "cancel"; id: string }
  | { type: "status" }
  | { type: "ping"; nonce?: string };

export type TurnCompleted = {
  type: "turn_completed";
  id: string;
  final_message: string;
  usage: TurnUsage | null;
  citations: readonly HistoryCitation[];
  usage_error?: string;
};

export type ActiveTurn = {
  id: string;
  input: PromptInput;
};

export type AgentCapabilities = Readonly<{
  durable_turns: true;
  resumable_events: true;
  live_steer: true;
  live_cancel: true;
  workspace: "cloudflare-computer";
  execution_environments: true;
  execution_namespace: "cwd-root-v1";
  native_cross_mounts: false;
}>;

export type ServerMessage = (
  | { type: "ready"; session_id: string; restored: boolean; active_turns: string[]; active_turn_details: ActiveTurn[]; capabilities: AgentCapabilities; latest_event_cursor: string; settings: ManagedAgentSettings }
  | { type: "agent_created"; agent_id: string; capabilities: AgentCapabilities }
  | { type: "turn_accepted"; id: string; input: PromptInput; replayed: boolean }
  | { type: "turn_cancelling"; id: string; error?: string; retry_at?: number }
  | TurnCompleted
  | { type: "turn_cancelled"; id: string }
  | { type: "turn_retryable"; id: string; error: string }
  | { type: "turn_failed"; id: string; error: string }
  | { type: "event"; event: AgentEvent; agent_id?: number }
  | { type: "stream_failed"; error: string }
  | { type: "status"; active_turns: string[]; active_turn_details: ActiveTurn[]; agent_loaded: boolean; connected_clients: number; settings: ManagedAgentSettings }
  | { type: "pong"; nonce?: string }
  | { type: "error"; code: string; message: string }
) & { cursor?: string; created_at?: number; turn_id?: string | null };

const TURN_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const IMAGE_DETAILS = new Set(["auto", "low", "high", "original"]);

export function parseCommand(encoded: string): ClientCommand {
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new ProtocolError("invalid_json", "messages must be JSON objects");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProtocolError("invalid_message", "messages must be JSON objects");
  }
  const command = value as Record<string, unknown>;
  if (command.type === "ping") {
    if (command.nonce !== undefined && typeof command.nonce !== "string") {
      throw new ProtocolError("invalid_nonce", "ping nonce must be a string");
    }
    return { type: "ping", ...(command.nonce === undefined ? {} : { nonce: command.nonce }) };
  }
  if (command.type === "status") return { type: "status" };
  if (!["prompt", "steer", "cancel"].includes(String(command.type))) {
    throw new ProtocolError("unknown_command", "supported commands are prompt, steer, cancel, status, and ping");
  }
  if (typeof command.id !== "string" || !TURN_ID.test(command.id)) {
    throw new ProtocolError("invalid_turn_id", "turn id must be 1-128 safe ASCII characters");
  }
  const type = command.type as "prompt" | "steer" | "cancel";
  if (command.type === "cancel") return { type: "cancel", id: command.id };
  validatePromptInput(command.input);
  return { type, id: command.id, input: command.input as PromptInput };
}

export function validatePromptInput(input: unknown): asserts input is PromptInput {
  if (typeof input === "string") {
    if (!input.trim()) throw new ProtocolError("empty_prompt", "prompt input must not be empty");
    return;
  }
  if (!Array.isArray(input) || input.length === 0) {
    throw new ProtocolError("invalid_prompt", "prompt input must be text or a non-empty content array");
  }
  for (const item of input) {
    if (!item || typeof item !== "object" || Array.isArray(item)) {
      throw new ProtocolError("invalid_prompt", "prompt content entries must be objects");
    }
    const value = item as Record<string, unknown>;
    if (value.type === "text") {
      exactKeys(value, ["type", "text"]);
      if (typeof value.text !== "string" || !value.text.trim()) {
        throw new ProtocolError("invalid_prompt", "text prompt entries require non-empty text");
      }
      continue;
    }
    if (value.type === "image") {
      exactKeys(value, ["type", "image_url", "detail"]);
      if (typeof value.image_url !== "string" || !value.image_url.trim()) {
        throw new ProtocolError("invalid_prompt", "image prompt entries require image_url");
      }
      if (value.detail !== undefined && !IMAGE_DETAILS.has(String(value.detail))) {
        throw new ProtocolError("invalid_prompt", "image detail must be auto, low, high, or original");
      }
      continue;
    }
    if (value.type === "audio") {
      exactKeys(value, ["type", "audio_url"]);
      if (typeof value.audio_url !== "string" || !value.audio_url.trim()) {
        throw new ProtocolError("invalid_prompt", "audio prompt entries require audio_url");
      }
      continue;
    }
    throw new ProtocolError("invalid_prompt", "prompt content supports text, image, and audio entries");
  }
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const fields = new Set(allowed);
  if (Object.keys(value).some((key) => !fields.has(key))) {
    throw new ProtocolError("invalid_prompt", `unsupported fields for ${String(value.type)} prompt entry`);
  }
}

export class ProtocolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}
