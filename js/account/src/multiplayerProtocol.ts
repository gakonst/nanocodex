export const MULTIPLAYER_MAX_MESSAGE_BYTES = 16 * 1024;
export const MULTIPLAYER_ROOM_ENDED_CLOSE_CODE = 4000;

export type MultiplayerTarget = "room" | "agent";

export type MultiplayerMember = Readonly<{
  id: string;
  name: string;
}>;

export type MultiplayerEvent =
  | { type: "member_joined"; member: MultiplayerMember }
  | {
      type: "member_message";
      id: string;
      member: MultiplayerMember;
      text: string;
      target: MultiplayerTarget;
    }
  | { type: "agent_message"; id: string; text: string; reply_to: string }
  | {
      type: "agent_error";
      id: string;
      code: "cancelled" | "failed" | "blocked" | "rate_limited";
      reply_to: string;
    };

export type MultiplayerServerMessage =
  | {
      type: "ready";
      room_id: string;
      member_id: string;
      members: MultiplayerMember[];
      online_member_ids: string[];
      latest_cursor: string;
      can_target_agent: boolean;
      can_end_room: boolean;
    }
  | {
      type: "room_event";
      cursor: string;
      created_at: number;
      event: MultiplayerEvent;
    }
  | { type: "accepted"; id: string; cursor: string; replayed: boolean }
  | { type: "replay_paused"; cursor: string; latest_cursor: string }
  | { type: "presence"; online_member_ids: string[] }
  | { type: "room_ended" }
  | { type: "pong"; nonce?: string }
  | { type: "error"; code: string; message: string; id?: string };

export type MultiplayerTimelineItem = Readonly<{
  cursor: string;
  createdAt: number;
  event: MultiplayerEvent;
}>;

export type MultiplayerRoomState = Readonly<{
  roomId: string;
  memberId: string;
  members: MultiplayerMember[];
  onlineMemberIds: string[];
  cursor: string;
  latestCursor: string;
  canTargetAgent: boolean;
  canEndRoom: boolean;
  timeline: MultiplayerTimelineItem[];
  inviteUrl?: string;
}>;

export type MultiplayerInvitation = Readonly<{
  roomId?: string;
  invite?: string;
}>;

export type MultiplayerCreateAttempt = Readonly<{
  createId: string;
  displayName: string;
}>;

export type MultiplayerJoinAttempt = Readonly<{
  roomId: string;
  invite: string;
  displayName: string;
  joinId: string;
}>;

export type MultiplayerPendingSend = Readonly<{
  roomId: string;
  memberId: string;
  id: string;
  encoded: string;
  text: string;
  target: MultiplayerTarget;
}>;

export type MultiplayerSessionStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const ROOM_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}~[A-Za-z0-9_-]{43}$/;
const TOKEN = /^[A-Za-z0-9_-]{43}$/;
const MESSAGE_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const CURSOR = /^(?:0|[1-9][0-9]*)$/;
const MAX_TIMELINE_ITEMS = 1_000;
const MAX_CREATE_STORAGE_BYTES = 256;
const MAX_JOIN_STORAGE_BYTES = 512;
const MAX_SEND_STORAGE_BYTES = (MULTIPLAYER_MAX_MESSAGE_BYTES * 6) + 2_048;
const CREATE_STORAGE_KEY = "nanocodex-multiplayer-create-v1";
const JOIN_STORAGE_PREFIX = "nanocodex-multiplayer-join-v1";
const SEND_STORAGE_PREFIX = "nanocodex-multiplayer-send-v1";
const storageEncoder = new TextEncoder();

export function createMultiplayerCreateAttempt(
  displayName: string,
): MultiplayerCreateAttempt {
  return {
    createId: randomCapability(),
    displayName: normalizedDisplayName(displayName),
  };
}

export function readMultiplayerCreateAttempt(
  storage: MultiplayerSessionStorage,
): MultiplayerCreateAttempt | undefined {
  const value = readStoredRecord(storage, CREATE_STORAGE_KEY, MAX_CREATE_STORAGE_BYTES);
  if (!value) return undefined;
  try {
    exactKeys(value, ["version", "create_id", "display_name"]);
    if (value.version !== 1) {
      throw new MultiplayerProtocolError("saved room creation changed version");
    }
    assertCreateId(value.create_id);
    return {
      createId: value.create_id,
      displayName: normalizedDisplayName(value.display_name),
    };
  } catch {
    removeStoredValue(storage, CREATE_STORAGE_KEY);
    return undefined;
  }
}

export function writeMultiplayerCreateAttempt(
  storage: MultiplayerSessionStorage,
  attempt: MultiplayerCreateAttempt,
): boolean {
  try {
    assertCreateId(attempt.createId);
    const displayName = normalizedDisplayName(attempt.displayName);
    const current = readMultiplayerCreateAttempt(storage);
    if (current && (
      current.createId !== attempt.createId
      || current.displayName !== displayName
    )) return false;
    const encoded = JSON.stringify({
      version: 1,
      create_id: attempt.createId,
      display_name: displayName,
    });
    if (storageEncoder.encode(encoded).byteLength > MAX_CREATE_STORAGE_BYTES) return false;
    storage.setItem(CREATE_STORAGE_KEY, encoded);
    return true;
  } catch {
    return false;
  }
}

export function clearMultiplayerCreateAttempt(
  storage: MultiplayerSessionStorage,
  attempt: MultiplayerCreateAttempt,
): void {
  const current = readMultiplayerCreateAttempt(storage);
  if (current?.createId === attempt.createId
    && current.displayName === attempt.displayName) {
    removeStoredValue(storage, CREATE_STORAGE_KEY);
  }
}

export function multiplayerInvitation(url: Pick<URL, "searchParams" | "hash">): MultiplayerInvitation {
  const room = url.searchParams.get("room") ?? undefined;
  const fragment = new URLSearchParams(url.hash.startsWith("#") ? url.hash.slice(1) : url.hash);
  const invite = fragment.get("invite") ?? undefined;
  return {
    roomId: room && ROOM_ID.test(room) ? room : undefined,
    invite: invite && TOKEN.test(invite) ? invite : undefined,
  };
}

export function multiplayerRoomPath(roomId?: string): string {
  if (!roomId) return "/multiplayer";
  assertRoomId(roomId);
  return `/multiplayer?room=${encodeURIComponent(roomId)}`;
}

export function multiplayerInviteUrl(origin: string, roomId: string, invite: string): string {
  assertRoomId(roomId);
  if (!TOKEN.test(invite)) throw new MultiplayerProtocolError("invalid room invitation");
  const url = new URL(multiplayerRoomPath(roomId), origin);
  url.hash = new URLSearchParams({ invite }).toString();
  return url.href;
}

export function multiplayerSocketUrl(origin: string, roomId: string, cursor: string): string {
  assertRoomId(roomId);
  assertCursor(cursor);
  const url = new URL(`/v1/rooms/${roomId}/ws`, origin);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("cursor", cursor);
  return url.href;
}

export function createMultiplayerJoinAttempt(
  roomId: string,
  invite: string,
  displayName: string,
): MultiplayerJoinAttempt {
  assertRoomId(roomId);
  if (!TOKEN.test(invite)) throw new MultiplayerProtocolError("invalid room invitation");
  return {
    roomId,
    invite,
    displayName: normalizedDisplayName(displayName),
    joinId: randomCapability(),
  };
}

export function readMultiplayerJoinAttempt(
  storage: MultiplayerSessionStorage,
  roomId: string,
  invite: string,
): MultiplayerJoinAttempt | undefined {
  assertRoomId(roomId);
  if (!TOKEN.test(invite)) return undefined;
  const key = joinStorageKey(roomId);
  const value = readStoredRecord(storage, key, MAX_JOIN_STORAGE_BYTES);
  if (!value) return undefined;
  try {
    exactKeys(value, ["version", "room_id", "invite", "display_name", "join_id"]);
    if (value.version !== 1 || value.room_id !== roomId || value.invite !== invite) {
      throw new MultiplayerProtocolError("saved join attempt changed identity");
    }
    assertJoinId(value.join_id);
    return {
      roomId,
      invite,
      displayName: normalizedDisplayName(value.display_name),
      joinId: value.join_id,
    };
  } catch {
    removeStoredValue(storage, key);
    return undefined;
  }
}

export function writeMultiplayerJoinAttempt(
  storage: MultiplayerSessionStorage,
  attempt: MultiplayerJoinAttempt,
): boolean {
  assertRoomId(attempt.roomId);
  if (!TOKEN.test(attempt.invite)) return false;
  try {
    assertJoinId(attempt.joinId);
    const encoded = JSON.stringify({
      version: 1,
      room_id: attempt.roomId,
      invite: attempt.invite,
      display_name: normalizedDisplayName(attempt.displayName),
      join_id: attempt.joinId,
    });
    if (storageEncoder.encode(encoded).byteLength > MAX_JOIN_STORAGE_BYTES) return false;
    storage.setItem(joinStorageKey(attempt.roomId), encoded);
    return true;
  } catch {
    return false;
  }
}

export function clearMultiplayerJoinAttempt(
  storage: MultiplayerSessionStorage,
  attempt: MultiplayerJoinAttempt,
): void {
  const current = readMultiplayerJoinAttempt(storage, attempt.roomId, attempt.invite);
  if (current?.joinId === attempt.joinId && current.displayName === attempt.displayName) {
    removeStoredValue(storage, joinStorageKey(attempt.roomId));
  }
}

export function createMultiplayerPendingSend(
  roomId: string,
  memberId: string,
  text: string,
  target: MultiplayerTarget,
): MultiplayerPendingSend {
  assertRoomId(roomId);
  assertMemberId(memberId);
  const normalized = text.trim();
  if (!normalized || storageEncoder.encode(normalized).byteLength > MULTIPLAYER_MAX_MESSAGE_BYTES) {
    throw new MultiplayerProtocolError("invalid room message text");
  }
  if (target !== "room" && target !== "agent") {
    throw new MultiplayerProtocolError("invalid room message target");
  }
  const id = `message-${randomCapability()}`;
  return {
    roomId,
    memberId,
    id,
    encoded: JSON.stringify({ type: "say", id, text: normalized, target }),
    text: normalized,
    target,
  };
}

export function readMultiplayerPendingSend(
  storage: MultiplayerSessionStorage,
  roomId: string,
): MultiplayerPendingSend | undefined {
  assertRoomId(roomId);
  const key = sendStorageKey(roomId);
  const value = readStoredRecord(storage, key, MAX_SEND_STORAGE_BYTES);
  if (!value) return undefined;
  try {
    exactKeys(value, ["version", "room_id", "member_id", "id", "encoded"]);
    if (value.version !== 1 || value.room_id !== roomId || typeof value.encoded !== "string") {
      throw new MultiplayerProtocolError("saved room command changed identity");
    }
    assertMemberId(value.member_id);
    assertMessageId(value.id);
    const command = decodeSayCommand(value.encoded);
    if (command.id !== value.id) {
      throw new MultiplayerProtocolError("saved room command id changed");
    }
    return {
      roomId,
      memberId: value.member_id,
      id: value.id,
      encoded: value.encoded,
      text: command.text,
      target: command.target,
    };
  } catch {
    removeStoredValue(storage, key);
    return undefined;
  }
}

export function writeMultiplayerPendingSend(
  storage: MultiplayerSessionStorage,
  pending: MultiplayerPendingSend,
): boolean {
  try {
    assertRoomId(pending.roomId);
    assertMemberId(pending.memberId);
    assertMessageId(pending.id);
    const command = decodeSayCommand(pending.encoded);
    if (command.id !== pending.id
      || command.text !== pending.text
      || command.target !== pending.target) return false;
    const encoded = JSON.stringify({
      version: 1,
      room_id: pending.roomId,
      member_id: pending.memberId,
      id: pending.id,
      encoded: pending.encoded,
    });
    if (storageEncoder.encode(encoded).byteLength > MAX_SEND_STORAGE_BYTES) return false;
    storage.setItem(sendStorageKey(pending.roomId), encoded);
    return true;
  } catch {
    return false;
  }
}

export function clearMultiplayerPendingSend(
  storage: MultiplayerSessionStorage,
  pending: MultiplayerPendingSend,
): void {
  const current = readMultiplayerPendingSend(storage, pending.roomId);
  if (current?.memberId === pending.memberId
    && current.id === pending.id
    && current.encoded === pending.encoded) {
    removeStoredValue(storage, sendStorageKey(pending.roomId));
  }
}

export function multiplayerPendingSendSettled(
  pending: MultiplayerPendingSend,
  message: MultiplayerServerMessage,
): boolean {
  if (message.type === "accepted") return message.id === pending.id;
  return message.type === "room_event"
    && message.event.type === "member_message"
    && message.event.id === pending.id
    && message.event.member.id === pending.memberId;
}

export function decodeMultiplayerMessage(encoded: string): MultiplayerServerMessage {
  let decoded: unknown;
  try {
    decoded = JSON.parse(encoded);
  } catch {
    throw new MultiplayerProtocolError("room sent invalid JSON");
  }
  const value = record(decoded);
  if (!value || typeof value.type !== "string") {
    throw new MultiplayerProtocolError("room sent an invalid message");
  }
  if (value.type === "ready") {
    exactKeys(value, [
      "type",
      "room_id",
      "member_id",
      "members",
      "online_member_ids",
      "latest_cursor",
      "can_target_agent",
      "can_end_room",
    ]);
    assertRoomId(value.room_id);
    assertMemberId(value.member_id);
    const members = memberArray(value.members);
    const onlineMemberIds = memberIdArray(value.online_member_ids);
    assertCursor(value.latest_cursor);
    if (typeof value.can_target_agent !== "boolean") {
      throw new MultiplayerProtocolError("room sent invalid agent authority");
    }
    if (typeof value.can_end_room !== "boolean") {
      throw new MultiplayerProtocolError("room sent invalid owner authority");
    }
    return {
      type: "ready",
      room_id: value.room_id,
      member_id: value.member_id,
      members,
      online_member_ids: onlineMemberIds,
      latest_cursor: value.latest_cursor,
      can_target_agent: value.can_target_agent,
      can_end_room: value.can_end_room,
    };
  }
  if (value.type === "room_event") {
    exactKeys(value, ["type", "cursor", "created_at", "event"]);
    assertCursor(value.cursor);
    if (typeof value.created_at !== "number" || !Number.isSafeInteger(value.created_at) || value.created_at < 0) {
      throw new MultiplayerProtocolError("room sent an invalid event time");
    }
    return {
      type: "room_event",
      cursor: value.cursor,
      created_at: value.created_at,
      event: decodeEvent(value.event),
    };
  }
  if (value.type === "accepted") {
    exactKeys(value, ["type", "id", "cursor", "replayed"]);
    assertMessageId(value.id);
    assertCursor(value.cursor);
    if (typeof value.replayed !== "boolean") {
      throw new MultiplayerProtocolError("room sent an invalid receipt");
    }
    return { type: "accepted", id: value.id, cursor: value.cursor, replayed: value.replayed };
  }
  if (value.type === "replay_paused") {
    exactKeys(value, ["type", "cursor", "latest_cursor"]);
    assertCursor(value.cursor);
    assertCursor(value.latest_cursor);
    if (BigInt(value.latest_cursor) <= BigInt(value.cursor)) {
      throw new MultiplayerProtocolError("room sent an invalid replay fence");
    }
    return {
      type: "replay_paused",
      cursor: value.cursor,
      latest_cursor: value.latest_cursor,
    };
  }
  if (value.type === "presence") {
    exactKeys(value, ["type", "online_member_ids"]);
    return { type: "presence", online_member_ids: memberIdArray(value.online_member_ids) };
  }
  if (value.type === "room_ended") {
    exactKeys(value, ["type"]);
    return { type: "room_ended" };
  }
  if (value.type === "pong") {
    exactKeys(value, ["type", "nonce"]);
    if (value.nonce !== undefined && typeof value.nonce !== "string") {
      throw new MultiplayerProtocolError("room sent an invalid pong");
    }
    return value.nonce === undefined
      ? { type: "pong" }
      : { type: "pong", nonce: value.nonce };
  }
  if (value.type === "error") {
    exactKeys(value, ["type", "code", "message", "id"]);
    if (!boundedString(value.code, 128) || !boundedString(value.message, 1_024)) {
      throw new MultiplayerProtocolError("room sent an invalid error");
    }
    if (value.id !== undefined) assertMessageId(value.id);
    return value.id === undefined
      ? { type: "error", code: value.code, message: value.message }
      : { type: "error", code: value.code, message: value.message, id: value.id };
  }
  throw new MultiplayerProtocolError("room sent an unknown message");
}

export function reduceMultiplayerMessage(
  state: MultiplayerRoomState,
  message: MultiplayerServerMessage,
): MultiplayerRoomState {
  if (message.type === "ready") {
    if (message.room_id !== state.roomId || BigInt(message.latest_cursor) < BigInt(state.cursor)) {
      throw new MultiplayerProtocolError("room replay identity changed");
    }
    return {
      ...state,
      memberId: message.member_id,
      members: message.members,
      onlineMemberIds: message.online_member_ids,
      latestCursor: message.latest_cursor,
      canTargetAgent: message.can_target_agent,
      canEndRoom: message.can_end_room,
    };
  }
  if (message.type === "presence") {
    return { ...state, onlineMemberIds: message.online_member_ids };
  }
  if (message.type === "replay_paused") {
    if (message.cursor !== state.cursor) {
      throw new MultiplayerProtocolError("room replay fence does not match the applied cursor");
    }
    return { ...state, latestCursor: message.latest_cursor };
  }
  if (message.type !== "room_event") return state;
  const current = BigInt(state.cursor);
  const incoming = BigInt(message.cursor);
  if (incoming <= current) return state;
  if (incoming !== current + 1n) {
    throw new MultiplayerProtocolError("room event cursor is not contiguous");
  }
  const members = message.event.type === "member_joined"
    ? upsertMember(state.members, message.event.member)
    : state.members;
  const timeline = [...state.timeline, {
    cursor: message.cursor,
    createdAt: message.created_at,
    event: message.event,
  }].slice(-MAX_TIMELINE_ITEMS);
  return {
    ...state,
    members,
    cursor: message.cursor,
    latestCursor: BigInt(state.latestCursor) > incoming ? state.latestCursor : message.cursor,
    timeline,
  };
}

export function createMultiplayerRoomState(
  ready: Extract<MultiplayerServerMessage, { type: "ready" }>,
  options: { cursor?: string; timeline?: MultiplayerTimelineItem[]; inviteUrl?: string } = {},
): MultiplayerRoomState {
  const cursor = options.cursor ?? "0";
  assertCursor(cursor);
  if (BigInt(cursor) > BigInt(ready.latest_cursor)) {
    throw new MultiplayerProtocolError("saved room cursor is ahead of the room");
  }
  return {
    roomId: ready.room_id,
    memberId: ready.member_id,
    members: ready.members,
    onlineMemberIds: ready.online_member_ids,
    cursor,
    latestCursor: ready.latest_cursor,
    canTargetAgent: ready.can_target_agent,
    canEndRoom: ready.can_end_room,
    timeline: options.timeline ?? [],
    ...(options.inviteUrl ? { inviteUrl: options.inviteUrl } : {}),
  };
}

export class MultiplayerProtocolError extends Error {}

function decodeEvent(value: unknown): MultiplayerEvent {
  const event = record(value);
  if (!event || typeof event.type !== "string") {
    throw new MultiplayerProtocolError("room sent an invalid event");
  }
  if (event.type === "member_joined") {
    exactKeys(event, ["type", "member"]);
    return { type: "member_joined", member: member(event.member) };
  }
  if (event.type === "member_message") {
    exactKeys(event, ["type", "id", "member", "text", "target"]);
    assertMessageId(event.id);
    if (!boundedString(event.text, MULTIPLAYER_MAX_MESSAGE_BYTES)
      || (event.target !== "room" && event.target !== "agent")) {
      throw new MultiplayerProtocolError("room sent an invalid member message");
    }
    return {
      type: "member_message",
      id: event.id,
      member: member(event.member),
      text: event.text,
      target: event.target,
    };
  }
  if (event.type === "agent_message") {
    exactKeys(event, ["type", "id", "text", "reply_to"]);
    assertMessageId(event.id);
    if (!boundedString(event.text, MULTIPLAYER_MAX_MESSAGE_BYTES) || !CURSOR.test(String(event.reply_to))) {
      throw new MultiplayerProtocolError("room sent an invalid agent message");
    }
    return { type: "agent_message", id: event.id, text: event.text, reply_to: String(event.reply_to) };
  }
  if (event.type === "agent_error") {
    exactKeys(event, ["type", "id", "code", "reply_to"]);
    assertMessageId(event.id);
    if (!["cancelled", "failed", "blocked", "rate_limited"].includes(String(event.code))
      || !CURSOR.test(String(event.reply_to))) {
      throw new MultiplayerProtocolError("room sent an invalid agent failure");
    }
    return {
      type: "agent_error",
      id: event.id,
      code: event.code as "cancelled" | "failed" | "blocked" | "rate_limited",
      reply_to: String(event.reply_to),
    };
  }
  throw new MultiplayerProtocolError("room sent an unknown event");
}

function memberArray(value: unknown): MultiplayerMember[] {
  if (!Array.isArray(value) || value.length > 64) {
    throw new MultiplayerProtocolError("room sent an invalid member list");
  }
  return value.map(member);
}

function memberIdArray(value: unknown): string[] {
  if (!Array.isArray(value) || value.length > 64) {
    throw new MultiplayerProtocolError("room sent an invalid presence list");
  }
  return value.map((id) => {
    assertMemberId(id);
    return id;
  });
}

function member(value: unknown): MultiplayerMember {
  const decoded = record(value);
  if (!decoded) throw new MultiplayerProtocolError("room sent an invalid member");
  exactKeys(decoded, ["id", "name"]);
  assertMemberId(decoded.id);
  if (!boundedString(decoded.name, 64) || !decoded.name.trim()) {
    throw new MultiplayerProtocolError("room sent an invalid display name");
  }
  return { id: decoded.id, name: decoded.name };
}

function upsertMember(members: MultiplayerMember[], incoming: MultiplayerMember): MultiplayerMember[] {
  const index = members.findIndex((candidate) => candidate.id === incoming.id);
  if (index === -1) return [...members, incoming];
  if (members[index]?.name === incoming.name) return members;
  return members.map((candidate) => candidate.id === incoming.id ? incoming : candidate);
}

function assertRoomId(value: unknown): asserts value is string {
  if (typeof value !== "string" || !ROOM_ID.test(value)) {
    throw new MultiplayerProtocolError("invalid room id");
  }
}

function assertMemberId(value: unknown): asserts value is string {
  if (typeof value !== "string" || !UUID.test(value)) {
    throw new MultiplayerProtocolError("invalid room member id");
  }
}

function assertMessageId(value: unknown): asserts value is string {
  if (typeof value !== "string" || !MESSAGE_ID.test(value)) {
    throw new MultiplayerProtocolError("invalid room message id");
  }
}

function assertJoinId(value: unknown): asserts value is string {
  if (typeof value !== "string" || !TOKEN.test(value)) {
    throw new MultiplayerProtocolError("invalid multiplayer join id");
  }
}

function assertCreateId(value: unknown): asserts value is string {
  if (typeof value !== "string" || !TOKEN.test(value)) {
    throw new MultiplayerProtocolError("invalid multiplayer create id");
  }
}

function assertCursor(value: unknown): asserts value is string {
  if (typeof value !== "string" || !CURSOR.test(value)) {
    throw new MultiplayerProtocolError("invalid room cursor");
  }
}

function boundedString(value: unknown, maxLength: number): value is string {
  return typeof value === "string" && value.length <= maxLength;
}

function normalizedDisplayName(value: unknown): string {
  if (typeof value !== "string") throw new MultiplayerProtocolError("invalid display name");
  const name = value.trim();
  if (!name || name !== value || storageEncoder.encode(name).byteLength > 64 || /\p{C}/u.test(name)) {
    throw new MultiplayerProtocolError("invalid display name");
  }
  return name;
}

function decodeSayCommand(encoded: string): {
  id: string;
  text: string;
  target: MultiplayerTarget;
} {
  if (storageEncoder.encode(encoded).byteLength > (MULTIPLAYER_MAX_MESSAGE_BYTES * 6) + 512) {
    throw new MultiplayerProtocolError("saved room command is too large");
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(encoded);
  } catch {
    throw new MultiplayerProtocolError("saved room command is invalid");
  }
  const command = record(decoded);
  if (!command) throw new MultiplayerProtocolError("saved room command is invalid");
  exactKeys(command, ["type", "id", "text", "target"]);
  if (command.type !== "say") throw new MultiplayerProtocolError("saved room command is invalid");
  assertMessageId(command.id);
  if (typeof command.text !== "string"
    || !command.text
    || command.text !== command.text.trim()
    || storageEncoder.encode(command.text).byteLength > MULTIPLAYER_MAX_MESSAGE_BYTES
    || (command.target !== "room" && command.target !== "agent")) {
    throw new MultiplayerProtocolError("saved room command is invalid");
  }
  return { id: command.id, text: command.text, target: command.target };
}

function randomCapability(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function joinStorageKey(roomId: string): string {
  return `${JOIN_STORAGE_PREFIX}:${roomId}`;
}

function sendStorageKey(roomId: string): string {
  return `${SEND_STORAGE_PREFIX}:${roomId}`;
}

function readStoredRecord(
  storage: MultiplayerSessionStorage,
  key: string,
  maxBytes: number,
): Record<string, unknown> | undefined {
  let encoded: string | null;
  try {
    encoded = storage.getItem(key);
  } catch {
    return undefined;
  }
  if (encoded === null) return undefined;
  if (storageEncoder.encode(encoded).byteLength > maxBytes) {
    removeStoredValue(storage, key);
    return undefined;
  }
  try {
    const value = record(JSON.parse(encoded));
    if (!value) throw new Error("stored value is not an object");
    return value;
  } catch {
    removeStoredValue(storage, key);
    return undefined;
  }
}

function removeStoredValue(storage: MultiplayerSessionStorage, key: string): void {
  try {
    storage.removeItem(key);
  } catch {
    // Session storage is a retry optimization; callers retain their in-memory value.
  }
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): void {
  const allowed = new Set(keys);
  if (Object.keys(value).some((key) => !allowed.has(key))) {
    throw new MultiplayerProtocolError("room sent unsupported fields");
  }
}
