import type { ChannelIdentity } from "./protocol.ts";

const MAX_CALLBACK_CHARS = 256_000;
const VIBER_SIGNATURE = /^[0-9a-f]{64}$/i;
const VIBER_USER_ID = /^[A-Za-z0-9+/=_-]{1,256}$/;

type Fetch = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export type ViberCallback = Readonly<{
  event: string;
  kind: "ignored";
}> | Readonly<{
  kind: "conversation_started";
  messageId: string;
  userId: string;
}> | Readonly<{
  actorId: string;
  kind: "message";
  messageId: string;
  text: string;
}>;

export async function readViberWebhook(request: Request, authToken: string): Promise<ViberCallback> {
  const signature = request.headers.get("x-viber-content-signature") ?? "";
  const body = await request.text();
  if (body.length > MAX_CALLBACK_CHARS || !await verifyViberSignature(body, signature, authToken)) {
    throw new ViberWebhookError("invalid_signature");
  }
  let payload: Record<string, unknown>;
  try {
    const decoded: unknown = JSON.parse(body);
    if (!isRecord(decoded)) throw new Error("payload must be an object");
    payload = decoded;
  } catch {
    throw new ViberWebhookError("invalid_payload");
  }
  const event = stringValue(payload.event);
  if (!event) throw new ViberWebhookError("invalid_payload");
  if (event === "conversation_started") {
    const user = isRecord(payload.user) ? payload.user : undefined;
    return {
      kind: "conversation_started",
      messageId: exactMessageToken(body),
      userId: requiredUserId(user?.id),
    };
  }
  if (event !== "message") return { event, kind: "ignored" };
  const sender = isRecord(payload.sender) ? payload.sender : undefined;
  const message = isRecord(payload.message) ? payload.message : undefined;
  if (!message) throw new ViberWebhookError("invalid_payload");
  const actorId = requiredUserId(sender?.id);
  return {
    actorId,
    kind: "message",
    messageId: exactMessageToken(body),
    text: messageText(message),
  };
}

export function viberChannelIdentity(
  botUri: string,
  userId: string,
): ChannelIdentity {
  if (!VIBER_USER_ID.test(userId)) throw new ViberWebhookError("invalid_payload");
  return {
    botUri,
    conversationId: `dm:${userId}`,
    platform: "viber",
    userId,
  };
}

export async function sendViberText(
  input: Readonly<{
    authToken: string;
    avatar?: string;
    botName: string;
    receiver: string;
    text: string;
    trackingData?: string;
  }>,
  fetcher: Fetch = fetch,
): Promise<void> {
  if (!VIBER_USER_ID.test(input.receiver)
    || input.botName.trim() !== input.botName
    || input.botName.length < 1
    || input.botName.length > 28
    || input.text.length < 1
    || input.text.length > 7_000
    || (input.trackingData?.length ?? 0) > 4_096
    || (input.avatar !== undefined && !isHttpsUrl(input.avatar))) {
    throw new ViberDeliveryError(0, "invalid_request");
  }
  const response = await fetcher("https://chatapi.viber.com/pa/send_message", {
    body: JSON.stringify({
      receiver: input.receiver,
      min_api_version: 1,
      sender: {
        name: input.botName,
        ...(input.avatar ? { avatar: input.avatar } : {}),
      },
      ...(input.trackingData ? { tracking_data: input.trackingData } : {}),
      type: "text",
      text: input.text,
    }),
    headers: {
      "content-type": "application/json",
      "x-viber-auth-token": input.authToken,
    },
    method: "POST",
  });
  let result: unknown;
  try { result = await response.json(); }
  catch { throw new ViberDeliveryError(response.status, "malformed_response"); }
  if (!response.ok || !isRecord(result) || result.status !== 0) {
    const code = isRecord(result) && typeof result.status === "number"
      ? `provider_status_${result.status}`
      : `http_${response.status}`;
    throw new ViberDeliveryError(response.status, code);
  }
}

export async function verifyViberSignature(
  body: string,
  signature: string,
  authToken: string,
): Promise<boolean> {
  if (!VIBER_SIGNATURE.test(signature)) return false;
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(authToken),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const expected = new Uint8Array(await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(body),
  ));
  const received = hexBytes(signature);
  let different = expected.length ^ received.length;
  for (let index = 0; index < expected.length; index += 1) {
    different |= expected[index]! ^ (received[index] ?? 0);
  }
  return different === 0;
}

export class ViberWebhookError extends Error {
  readonly code: "invalid_payload" | "invalid_signature";

  constructor(code: "invalid_payload" | "invalid_signature") {
    super(code);
    this.code = code;
  }
}

export class ViberDeliveryError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(status: number, code: string) {
    super(code);
    this.code = code;
    this.status = status;
  }
}

function exactMessageToken(body: string): string {
  const match = /"message_token"\s*:\s*(?:"([0-9]+)"|([0-9]+))/.exec(body);
  const value = match?.[1] ?? match?.[2];
  if (!value) throw new ViberWebhookError("invalid_payload");
  return value;
}

function requiredUserId(value: unknown): string {
  if (typeof value !== "string" || !VIBER_USER_ID.test(value)) {
    throw new ViberWebhookError("invalid_payload");
  }
  return value;
}

function messageText(message: Record<string, unknown>): string {
  const type = stringValue(message.type);
  if (!type) throw new ViberWebhookError("invalid_payload");
  const text = stringValue(message.text)?.trim();
  switch (type) {
    case "text":
      if (!text) throw new ViberWebhookError("invalid_payload");
      return text;
    case "picture": return joined("[Viber image]", text, urlValue(message.media));
    case "video": return joined("[Viber video]", text, urlValue(message.media));
    case "file": return joined(
      `[Viber file${stringValue(message.file_name) ? `: ${stringValue(message.file_name)}` : ""}]`,
      text,
      urlValue(message.media),
    );
    case "url": return joined("[Viber URL]", text, urlValue(message.media));
    case "sticker": return `[Viber sticker${numberValue(message.sticker_id) !== undefined
      ? `: ${numberValue(message.sticker_id)}` : ""}]`;
    case "location": {
      const location = isRecord(message.location) ? message.location : undefined;
      const lat = numberValue(location?.lat);
      const lon = numberValue(location?.lon);
      if (lat === undefined || lon === undefined) throw new ViberWebhookError("invalid_payload");
      return `[Viber location: ${lat}, ${lon}]`;
    }
    case "contact": {
      const contact = isRecord(message.contact) ? message.contact : undefined;
      const name = stringValue(contact?.name);
      const phone = stringValue(contact?.phone_number);
      if (!name && !phone) throw new ViberWebhookError("invalid_payload");
      return joined("[Viber contact]", name, phone);
    }
    default: return `[Unsupported Viber message: ${type}]`;
  }
}

function joined(...parts: (string | undefined)[]): string {
  return parts.filter((part): part is string => Boolean(part)).join("\n");
}

function urlValue(value: unknown): string | undefined {
  const candidate = stringValue(value);
  if (!candidate) return undefined;
  try {
    const url = new URL(candidate);
    return url.protocol === "https:" ? url.href : undefined;
  } catch {
    return undefined;
  }
}

function isHttpsUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password;
  } catch {
    return false;
  }
}

function hexBytes(value: string): Uint8Array {
  const bytes = new Uint8Array(value.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function numberValue(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) return Number(value);
  return undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}
