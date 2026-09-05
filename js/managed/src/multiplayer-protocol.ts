export const MAX_ROOM_MESSAGE_BYTES = 16 * 1024;
export const MAX_DISPLAY_NAME_BYTES = 64;
export const ROOM_ENDED_CLOSE_CODE = 4000;

export type RoomTarget = "room" | "agent";

export type RoomClientCommand =
  | { type: "say"; id: string; text: string; target: RoomTarget }
  | { type: "ack"; cursor: string }
  | { type: "ping"; nonce?: string };

export type RoomMember = Readonly<{
  id: string;
  name: string;
}>;

export type RoomEventMessage =
  | { type: "member_joined"; member: RoomMember }
  | {
      type: "member_message";
      id: string;
      member: RoomMember;
      text: string;
      target: RoomTarget;
    }
  | {
      type: "agent_message";
      id: string;
      text: string;
      reply_to: string;
    }
  | {
      type: "agent_error";
      id: string;
      code: "cancelled" | "failed" | "blocked" | "rate_limited";
      reply_to: string;
    };

export type RoomServerMessage =
  | {
      type: "ready";
      room_id: string;
      member_id: string;
      members: RoomMember[];
      online_member_ids: string[];
      latest_cursor: string;
      can_target_agent: boolean;
      can_end_room: boolean;
    }
  | {
      type: "room_event";
      cursor: string;
      created_at: number;
      event: RoomEventMessage;
    }
  | { type: "accepted"; id: string; cursor: string; replayed: boolean }
  | { type: "replay_paused"; cursor: string; latest_cursor: string }
  | { type: "presence"; online_member_ids: string[] }
  | { type: "room_ended" }
  | { type: "pong"; nonce?: string }
  | { type: "error"; code: string; message: string; id?: string };

const MESSAGE_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const RECEIPT_ID = /^[A-Za-z0-9_-]{43}$/;
const CURSOR = /^(0|[1-9][0-9]{0,18})$/;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function truncateRoomMessage(text: string): string {
  const encoded = encoder.encode(text);
  if (encoded.byteLength <= MAX_ROOM_MESSAGE_BYTES) return text;
  const suffix = "…";
  let end = MAX_ROOM_MESSAGE_BYTES - encoder.encode(suffix).byteLength;
  while (end > 0 && (encoded[end]! & 0xc0) === 0x80) end -= 1;
  return `${decoder.decode(encoded.subarray(0, end))}${suffix}`;
}

export function parseRoomCommand(encoded: string): RoomClientCommand {
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new RoomProtocolError("invalid_json", "messages must be JSON objects");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new RoomProtocolError("invalid_message", "messages must be JSON objects");
  }
  const command = value as Record<string, unknown>;
  if (command.type === "ping") {
    exactKeys(command, ["type", "nonce"]);
    if (command.nonce !== undefined && typeof command.nonce !== "string") {
      throw new RoomProtocolError("invalid_nonce", "ping nonce must be a string");
    }
    return command.nonce === undefined
      ? { type: "ping" }
      : { type: "ping", nonce: command.nonce };
  }
  if (command.type === "ack") {
    exactKeys(command, ["type", "cursor"]);
    if (typeof command.cursor !== "string" || !CURSOR.test(command.cursor)) {
      throw new RoomProtocolError("invalid_cursor", "replay acknowledgement cursor is invalid");
    }
    return { type: "ack", cursor: command.cursor };
  }
  if (command.type !== "say") {
    throw new RoomProtocolError("unknown_command", "supported commands are say, ack, and ping");
  }
  exactKeys(command, ["type", "id", "text", "target"]);
  if (typeof command.id !== "string" || !MESSAGE_ID.test(command.id)) {
    throw new RoomProtocolError("invalid_message_id", "message id must be 1-128 safe ASCII characters");
  }
  if (typeof command.text !== "string" || !command.text.trim()) {
    throw new RoomProtocolError("empty_message", "message text must not be empty");
  }
  if (encoder.encode(command.text).byteLength > MAX_ROOM_MESSAGE_BYTES) {
    throw new RoomProtocolError("message_too_large", "message text exceeds 16 KiB");
  }
  if (command.target !== "room" && command.target !== "agent") {
    throw new RoomProtocolError("invalid_target", "message target must be room or agent");
  }
  return {
    type: "say",
    id: command.id,
    text: command.text.trim(),
    target: command.target,
  };
}

export function validateDisplayName(value: unknown): string {
  if (typeof value !== "string") {
    throw new RoomProtocolError("invalid_display_name", "display name must be text");
  }
  const name = value.trim();
  if (!name || encoder.encode(name).byteLength > MAX_DISPLAY_NAME_BYTES) {
    throw new RoomProtocolError(
      "invalid_display_name",
      "display name must be 1-64 UTF-8 bytes",
    );
  }
  if (/\p{C}/u.test(name)) {
    throw new RoomProtocolError("invalid_display_name", "display name contains control characters");
  }
  return name;
}

export function validateJoinId(value: unknown): string {
  if (typeof value !== "string" || !RECEIPT_ID.test(value)) {
    throw new RoomProtocolError(
      "invalid_join_id",
      "join id must be a 32-byte base64url value",
    );
  }
  return value;
}

export function validateCreateId(value: unknown): string {
  if (typeof value !== "string" || !RECEIPT_ID.test(value)) {
    throw new RoomProtocolError(
      "invalid_create_id",
      "create id must be a 32-byte base64url value",
    );
  }
  return value;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const fields = new Set(allowed);
  if (Object.keys(value).some((key) => !fields.has(key))) {
    throw new RoomProtocolError("invalid_message", "message contains unsupported fields");
  }
}

export class RoomProtocolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}
