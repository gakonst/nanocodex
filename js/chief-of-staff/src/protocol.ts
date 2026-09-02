import type { SlackWebhookPayload } from "@chat-adapter/slack/webhook";

const SLACK_TEAM_ID = /^T[A-Z0-9]+$/;
const SLACK_CHANNEL_ID = /^[CDG][A-Z0-9]+$/;
const SLACK_USER_ID = /^[UW][A-Z0-9]+$/;
const SLACK_THREAD_ID = /^slack:[CDG][A-Z0-9]+:(?:[0-9]+\.[0-9]+)?$/;
const API_KEY = /^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/;
const SLACK_CLIENT_ID = /^[0-9]+\.[0-9]+$/;
const BASE64_URL = /^[A-Za-z0-9_-]+$/;

export type ChannelIdentity = Readonly<{
  accountId: string;
  channelId: string;
  conversationId: string;
  platform: "slack";
  teamId: string;
}>;

export type SlackMessageIdentity = Readonly<{
  actorId: string;
  channel: ChannelIdentity;
  messageId: string;
}>;

export type Readiness = Readonly<{
  accountMatch: boolean;
  channels: readonly Readonly<{
    id: "slack" | "whatsapp" | "imessage";
    availability: "ready" | "setup_required" | "not_enabled";
    contract: "first_party" | "vendor_official";
    detail: string;
  }>[];
  configured: boolean;
  installations: readonly SlackInstallationSummary[];
  installUrl: string | null;
  webhookUrl: string | null;
}>;

export type SlackInstallationMetadata = Readonly<{
  accountId: string;
  botUserId: string | null;
  installedAt: number;
  teamId: string;
  teamName: string;
}>;

export type SlackInstallationSummary = Omit<SlackInstallationMetadata, "accountId">;

export function slackTeamId(payload: SlackWebhookPayload): string | undefined {
  if ("teamId" in payload && SLACK_TEAM_ID.test(payload.teamId ?? "")) {
    return payload.teamId;
  }
  if (!isRecord(payload.raw)) return undefined;
  const event = isRecord(payload.raw.event) ? payload.raw.event : undefined;
  const candidate = stringValue(event?.team_id) ?? stringValue(event?.team)
    ?? stringValue(payload.raw.team_id);
  return candidate && SLACK_TEAM_ID.test(candidate) ? candidate : undefined;
}

export function slackMessageIdentity(
  raw: unknown,
  threadId: string,
  isDirectMessage: boolean,
  accountId: string,
): SlackMessageIdentity {
  if (!isRecord(raw)) throw new Error("Slack message payload is missing");
  const teamId = stringValue(raw.team_id) ?? stringValue(raw.team);
  const channelId = stringValue(raw.channel);
  const actorId = stringValue(raw.user);
  const messageId = stringValue(raw.ts);
  if (!teamId || !SLACK_TEAM_ID.test(teamId)) throw new Error("Slack team identity is invalid");
  if (!channelId || !SLACK_CHANNEL_ID.test(channelId)) {
    throw new Error("Slack channel identity is invalid");
  }
  if (!actorId || !SLACK_USER_ID.test(actorId)) throw new Error("Slack user identity is invalid");
  if (!messageId || !/^[0-9]+\.[0-9]+$/.test(messageId)) {
    throw new Error("Slack message identity is invalid");
  }
  if (!SLACK_THREAD_ID.test(threadId)) throw new Error("Slack thread identity is invalid");
  const expectedPrefix = `slack:${channelId}:`;
  if (!threadId.startsWith(expectedPrefix)) throw new Error("Slack thread is not bound to its channel");
  return {
    actorId,
    messageId,
    channel: {
      accountId,
      channelId,
      conversationId: isDirectMessage ? `dm:${actorId}` : threadId,
      platform: "slack",
      teamId,
    },
  };
}

export function configurationReadiness(env: {
  CHIEF_OF_STAFF_PUBLIC_ORIGIN?: string;
  NANOCODEX_API_KEY?: string;
  SLACK_CLIENT_ID?: string;
  SLACK_CLIENT_SECRET?: string;
  SLACK_ENCRYPTION_KEY?: string;
  SLACK_OAUTH_STATE_SECRET?: string;
  SLACK_SIGNING_SECRET?: string;
}): Readonly<{ configured: boolean; webhookUrl: string | null }> {
  let origin: URL | undefined;
  try {
    origin = env.CHIEF_OF_STAFF_PUBLIC_ORIGIN
      ? new URL(env.CHIEF_OF_STAFF_PUBLIC_ORIGIN)
      : undefined;
  } catch {
    origin = undefined;
  }
  const configured = Boolean(
    origin?.protocol === "https:"
    && origin.pathname === "/"
    && !origin.search
    && !origin.hash
    && API_KEY.test(env.NANOCODEX_API_KEY ?? "")
    && SLACK_CLIENT_ID.test(env.SLACK_CLIENT_ID ?? "")
    && (env.SLACK_CLIENT_SECRET?.length ?? 0) >= 16
    && validBase64Key(env.SLACK_ENCRYPTION_KEY)
    && validBase64Key(env.SLACK_OAUTH_STATE_SECRET)
    && (env.SLACK_SIGNING_SECRET?.length ?? 0) >= 16
  );
  return {
    configured,
    webhookUrl: origin ? new URL("/webhooks/slack", origin).href : null,
  };
}

export function validSlackInstallationMetadata(value: unknown): value is SlackInstallationMetadata {
  return isRecord(value)
    && typeof value.accountId === "string"
    && value.accountId.length > 0
    && (value.botUserId === null || (typeof value.botUserId === "string" && SLACK_USER_ID.test(value.botUserId)))
    && typeof value.installedAt === "number"
    && Number.isSafeInteger(value.installedAt)
    && value.installedAt > 0
    && typeof value.teamId === "string"
    && SLACK_TEAM_ID.test(value.teamId)
    && typeof value.teamName === "string"
    && value.teamName.trim().length > 0
    && value.teamName.length <= 200;
}

export async function digest(value: unknown): Promise<string> {
  const encoded = new TextEncoder().encode(canonicalJson(value));
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", encoded));
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function sameChannelIdentity(left: ChannelIdentity, right: ChannelIdentity): boolean {
  return left.accountId === right.accountId
    && left.channelId === right.channelId
    && left.conversationId === right.conversationId
    && left.platform === right.platform
    && left.teamId === right.teamId;
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  const record = value as Record<string, unknown>;
  return `{${Object.keys(record).sort().map((key) =>
    `${JSON.stringify(key)}:${canonicalJson(record[key])}`
  ).join(",")}}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

function validBase64Key(value: unknown): boolean {
  if (typeof value !== "string" || !BASE64_URL.test(value)) return false;
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    return atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "=")).length === 32;
  } catch {
    return false;
  }
}
