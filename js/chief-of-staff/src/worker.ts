import { createSlackAdapter, type SlackAdapter } from "@chat-adapter/slack";
import {
  readSlackWebhook,
  type SlackWebhookPayload,
} from "@chat-adapter/slack/webhook";
import { Chat, type Message, type Thread } from "chat";
import { NanocodexManagedGateway, requestingAccountId } from "./managed.ts";
import {
  configurationReadiness,
  digest,
  slackMessageIdentity,
  slackTeamId,
  validSlackInstallationMetadata,
  type Readiness,
  type SlackInstallationMetadata,
} from "./protocol.ts";
import { slackAuthorizationUrl, verifySlackInstallState } from "./slack-oauth.ts";
import { DurableChatStateAdapter } from "./state-adapter.ts";
import {
  readViberWebhook,
  sendViberText,
  ViberWebhookError,
  viberChannelIdentity,
} from "./viber.ts";

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
type StateNamespace = Readonly<{ getByName(name: string): Fetcher }>;

export interface Env {
  CHIEF_OF_STAFF_ACCOUNT_ORIGIN?: string;
  CHIEF_OF_STAFF_PUBLIC_ORIGIN?: string;
  CHIEF_OF_STAFF_STATE: StateNamespace;
  NANOCODEX_API_KEY?: string;
  NANOCODEX_BACKEND?: Fetcher;
  SLACK_API_URL?: string;
  SLACK_CLIENT_ID?: string;
  SLACK_CLIENT_SECRET?: string;
  SLACK_ENCRYPTION_KEY?: string;
  SLACK_OAUTH_STATE_SECRET?: string;
  SLACK_SIGNING_SECRET?: string;
  VIBER_AUTH_TOKEN?: string;
  VIBER_BOT_AVATAR?: string;
  VIBER_BOT_NAME?: string;
  VIBER_BOT_URI?: string;
}

type ChiefRuntime = Readonly<{ chat: Chat<{ slack: SlackAdapter }>; slack: SlackAdapter }>;

const runtimes = new WeakMap<object, ChiefRuntime>();
const ownerIds = new WeakMap<object, Promise<string | undefined>>();
const INSTALLATIONS_OBJECT = "chat-sdk:slack";
const SLACK_TEAM_ID = /^T[A-Z0-9]+$/;

export default {
  async fetch(request: Request, env: Env, context?: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health" && request.method === "GET") {
      return json({ service: "nanocodex-chief-of-staff", status: "ok" });
    }
    if (url.pathname === "/v1/readiness" && request.method === "GET") {
      return readiness(request, env);
    }
    if (url.pathname === "/v1/slack/install") {
      if (request.method !== "GET") return methodNotAllowed(["GET"]);
      return beginSlackInstall(request, env);
    }
    if (url.pathname === "/v1/slack/callback") {
      if (request.method !== "GET") return methodNotAllowed(["GET"]);
      return finishSlackInstall(request, env);
    }
    const slackInstallation = url.pathname.match(/^\/v1\/slack\/installations\/(T[A-Z0-9]+)$/);
    if (slackInstallation) {
      if (request.method !== "DELETE") return methodNotAllowed(["DELETE"]);
      return removeSlackInstallation(request, env, slackInstallation[1]!);
    }
    if (url.pathname === "/webhooks/slack") {
      if (request.method !== "POST") return methodNotAllowed(["POST"]);
      return slackWebhook(request, env, context);
    }
    if (url.pathname === "/webhooks/viber") {
      if (request.method !== "POST") return methodNotAllowed(["POST"]);
      return viberWebhook(request, env);
    }
    return json({ error: "not_found" }, { status: 404 });
  },
};

async function beginSlackInstall(request: Request, env: Env): Promise<Response> {
  const config = configurationReadiness(env);
  if (!config.configured || !env.NANOCODEX_BACKEND || !env.SLACK_CLIENT_ID
    || !env.SLACK_OAUTH_STATE_SECRET || !env.CHIEF_OF_STAFF_PUBLIC_ORIGIN) {
    return json({ error: "slack_app_not_configured" }, { status: 503 });
  }
  const requester = await requestingAccountId(env.NANOCODEX_BACKEND, request);
  if (!requester) return json({ error: "unauthorized" }, { status: 401 });
  if (await configuredOwnerId(env) !== requester) {
    return json({ error: "account_forbidden" }, { status: 403 });
  }
  const redirectUri = new URL("/v1/slack/callback", env.CHIEF_OF_STAFF_PUBLIC_ORIGIN).href;
  const authorization = await slackAuthorizationUrl({
    accountId: requester,
    clientId: env.SLACK_CLIENT_ID,
    redirectUri,
    stateSecret: env.SLACK_OAUTH_STATE_SECRET,
  });
  return new Response(null, {
    status: 302,
    headers: {
      "cache-control": "no-store",
      location: authorization.href,
      "referrer-policy": "no-referrer",
    },
  });
}

async function finishSlackInstall(request: Request, env: Env): Promise<Response> {
  const config = configurationReadiness(env);
  if (!config.configured || !env.SLACK_OAUTH_STATE_SECRET || !env.CHIEF_OF_STAFF_PUBLIC_ORIGIN) {
    return json({ error: "slack_app_not_configured" }, { status: 503 });
  }
  const url = new URL(request.url);
  if (url.searchParams.has("error")) return installationReturn(env, "cancelled");
  const state = await verifySlackInstallState(
    url.searchParams.get("state") ?? "",
    env.SLACK_OAUTH_STATE_SECRET,
  );
  if (!state || await configuredOwnerId(env) !== state.accountId) {
    return json({ error: "invalid_oauth_state" }, { status: 400 });
  }
  try {
    const runtime = chiefRuntime(env);
    await runtime.chat.initialize();
    const result = await runtime.slack.handleOAuthCallback(request, {
      redirectUri: new URL("/v1/slack/callback", env.CHIEF_OF_STAFF_PUBLIC_ORIGIN).href,
    });
    if (result.isEnterpriseInstall || !SLACK_TEAM_ID.test(result.teamId)) {
      await runtime.slack.deleteInstallation(result.teamId);
      return installationReturn(env, "workspace_required");
    }
    const metadata = {
      accountId: state.accountId,
      botUserId: result.installation.botUserId ?? null,
      installedAt: Date.now(),
      teamId: result.teamId,
      teamName: result.installation.teamName?.trim() || result.teamId,
    } satisfies SlackInstallationMetadata;
    await writeInstallationMetadata(env, metadata, "PUT");
    return installationReturn(env, "installed");
  } catch (error) {
    console.warn({
      type: "chief_of_staff.slack_install_failed",
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return installationReturn(env, "failed");
  }
}

async function removeSlackInstallation(
  request: Request,
  env: Env,
  teamId: string,
): Promise<Response> {
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  const requester = await requestingAccountId(env.NANOCODEX_BACKEND, request);
  if (!requester) return json({ error: "unauthorized" }, { status: 401 });
  const metadata = await installationMetadata(env, teamId);
  if (!metadata) return json({ error: "not_found" }, { status: 404 });
  if (metadata.accountId !== requester || await configuredOwnerId(env) !== requester) {
    return json({ error: "account_forbidden" }, { status: 403 });
  }
  const runtime = chiefRuntime(env);
  await runtime.chat.initialize();
  const installation = await runtime.slack.getInstallation(teamId);
  if (installation && env.SLACK_CLIENT_ID && env.SLACK_CLIENT_SECRET) {
    await runtime.slack.withBotToken(
      installation.botToken,
      () => runtime.slack.webClient.apps.uninstall({
        client_id: env.SLACK_CLIENT_ID!,
        client_secret: env.SLACK_CLIENT_SECRET!,
      }),
      { installationId: teamId },
    );
  }
  await runtime.slack.deleteInstallation(teamId);
  await writeInstallationMetadata(env, metadata, "DELETE");
  return new Response(null, {
    status: 204,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}

async function slackWebhook(
  request: Request,
  env: Env,
  context?: ExecutionContext,
): Promise<Response> {
  const config = configurationReadiness(env);
  if (!config.slack.configured || !env.SLACK_SIGNING_SECRET) {
    return json({ error: "channel_not_configured" }, { status: 503 });
  }
  let payload: SlackWebhookPayload;
  try {
    payload = await readSlackWebhook(request.clone(), {
      signingSecret: env.SLACK_SIGNING_SECRET,
    });
  } catch {
    return new Response("Invalid signature", {
      status: 401,
      headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
    });
  }
  if (payload.kind === "url_verification") {
    return json({ challenge: payload.challenge });
  }
  const teamId = slackTeamId(payload);
  const metadata = teamId ? await installationMetadata(env, teamId) : undefined;
  if (!metadata || metadata.accountId !== await configuredOwnerId(env)) {
    return json({ error: "workspace_forbidden" }, { status: 403 });
  }
  if (slackEventType(payload) === "app_uninstalled") {
    const runtime = chiefRuntime(env);
    await runtime.chat.initialize();
    await runtime.slack.deleteInstallation(teamId!);
    await writeInstallationMetadata(env, metadata, "DELETE");
    return json({ ok: true });
  }
  try {
    return await chiefRuntime(env).chat.webhooks.slack(
      request as unknown as Parameters<Chat["webhooks"]["slack"]>[0],
      {
        waitUntil(task) {
          const handled = task.catch((error) => {
            console.warn({
              type: "chief_of_staff.slack_delivery_failed",
              error_kind: error instanceof Error ? error.name : typeof error,
            });
          });
          if (context) context.waitUntil(handled);
        },
      },
    );
  } catch (error) {
    console.warn({
      type: "chief_of_staff.slack_webhook_failed",
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "channel_unavailable" }, { status: 503 });
  }
}

async function viberWebhook(request: Request, env: Env): Promise<Response> {
  const config = configurationReadiness(env);
  if (!config.viber.configured || !env.VIBER_AUTH_TOKEN || !env.VIBER_BOT_NAME
    || !env.VIBER_BOT_URI) {
    return json({ error: "channel_not_configured" }, { status: 503 });
  }
  let callback;
  try {
    callback = await readViberWebhook(request, env.VIBER_AUTH_TOKEN);
  } catch (error) {
    if (error instanceof ViberWebhookError) {
      return json({ error: error.code }, { status: error.code === "invalid_signature" ? 401 : 400 });
    }
    return json({ error: "invalid_payload" }, { status: 400 });
  }
  if (callback.kind === "ignored") return json({ status: 0 });
  const ownerId = await configuredOwnerId(env);
  if (!ownerId) return json({ error: "channel_unavailable" }, { status: 503 });
  const receiver = callback.kind === "message" ? callback.actorId : callback.userId;
  const identity = viberChannelIdentity(ownerId, env.VIBER_BOT_URI, receiver);
  const routeDigest = await digest(identity);
  const session = env.CHIEF_OF_STAFF_STATE.getByName(`conversation:${routeDigest}`);
  if (callback.kind === "conversation_started") {
    try {
      await deliverViberOnce(session, `viber:welcome:${callback.messageId}`, async () => {
        await sendViberText({
          authToken: env.VIBER_AUTH_TOKEN!,
          avatar: env.VIBER_BOT_AVATAR,
          botName: env.VIBER_BOT_NAME!,
          receiver: callback.userId,
          text: "Hi — I’m your Nanocodex Chief of Staff. Send me a message to get started.",
          trackingData: `welcome:${callback.messageId}`,
        });
      });
      return json({ status: 0 });
    } catch (error) {
      logViberFailure("welcome_failed", error);
      return json({ error: "channel_unavailable" }, { status: 503 });
    }
  }
  try {
    const response = await session.fetch(new Request("https://state.internal/conversation/turn", {
      body: JSON.stringify({
        actorId: callback.actorId,
        channel: identity,
        messageId: callback.messageId,
        text: callback.text,
      }),
      headers: { "content-type": "application/json" },
      method: "POST",
    }));
    if (!response.ok) throw new Error(`Chief of Staff turn failed (${response.status})`);
    const result = await response.json<{ finalMessage?: unknown }>();
    if (typeof result.finalMessage !== "string") throw new Error("Chief of Staff turn was malformed");
    const finalMessage = result.finalMessage;
    await deliverViberOnce(session, `viber:reply:${callback.messageId}`, async () => {
      await sendViberText({
        authToken: env.VIBER_AUTH_TOKEN!,
        avatar: env.VIBER_BOT_AVATAR,
        botName: env.VIBER_BOT_NAME!,
        receiver: callback.actorId,
        text: finalMessage,
        trackingData: `reply-to:${callback.messageId}`,
      });
    });
    return json({ status: 0 });
  } catch (error) {
    logViberFailure("message_failed", error);
    return json({ error: "channel_unavailable" }, { status: 503 });
  }
}

async function deliverViberOnce(
  session: Fetcher,
  deliveryId: string,
  send: () => Promise<void>,
): Promise<void> {
  const claimResponse = await deliveryRequest(session, { deliveryId, operation: "claim" });
  if (!claimResponse.ok) throw new Error(`Viber delivery claim failed (${claimResponse.status})`);
  const claim = await claimResponse.json<{ status?: unknown; token?: unknown }>();
  if (claim.status === "completed") return;
  if (claim.status !== "claimed" || typeof claim.token !== "string") {
    throw new Error("Viber delivery is already in progress");
  }
  try {
    await send();
  } catch (error) {
    try {
      await deliveryRequest(session, { deliveryId, operation: "release", token: claim.token });
    } catch { /* The expiring claim permits a later provider retry. */ }
    throw error;
  }
  const completed = await deliveryRequest(session, {
    deliveryId,
    operation: "complete",
    token: claim.token,
  });
  if (!completed.ok) {
    console.warn({ type: "chief_of_staff.viber_delivery_checkpoint_failed" });
  }
}

function deliveryRequest(
  session: Fetcher,
  body: Readonly<{ deliveryId: string; operation: "claim" | "complete" | "release"; token?: string }>,
): Promise<Response> {
  return session.fetch(new Request("https://state.internal/conversation/delivery", {
    body: JSON.stringify(body),
    headers: { "content-type": "application/json" },
    method: "POST",
  }));
}

function chiefRuntime(env: Env): ChiefRuntime {
  const retained = runtimes.get(env as object);
  if (retained) return retained;
  const stateObject = env.CHIEF_OF_STAFF_STATE.getByName(INSTALLATIONS_OBJECT);
  const slack = createSlackAdapter({
    agentView: true,
    apiUrl: env.SLACK_API_URL,
    clientId: env.SLACK_CLIENT_ID,
    clientSecret: env.SLACK_CLIENT_SECRET,
    encryptionKey: env.SLACK_ENCRYPTION_KEY,
    logger: undefined,
    signingSecret: undefined,
    webhookVerifier: () => true,
  });
  const chat = new Chat({
    adapters: { slack },
    concurrency: "concurrent",
    dedupeTtlMs: 24 * 60 * 60 * 1_000,
    logger: "warn",
    state: new DurableChatStateAdapter(stateObject),
    userName: "chief-of-staff",
  });
  const handle = async (thread: Thread, message: Message) => {
    await thread.subscribe();
    const teamId = messageTeamId(message.raw);
    const metadata = teamId ? await installationMetadata(env, teamId) : undefined;
    if (!metadata || metadata.accountId !== await configuredOwnerId(env)) {
      throw new Error("Slack installation owner is unavailable");
    }
    const identity = slackMessageIdentity(message.raw, thread.id, thread.isDM, metadata.accountId);
    const routeDigest = await digest(identity.channel);
    const session = env.CHIEF_OF_STAFF_STATE.getByName(`conversation:${routeDigest}`);
    try {
      await thread.startTyping("Working");
    } catch { /* A Slack surface can omit Agent Session status support. */ }
    const response = await session.fetch(new Request("https://state.internal/conversation/turn", {
      body: JSON.stringify({
        actorId: identity.actorId,
        channel: identity.channel,
        messageId: identity.messageId,
        text: message.text,
      }),
      headers: { "content-type": "application/json" },
      method: "POST",
    }));
    if (!response.ok) throw new Error(`Chief of Staff turn failed (${response.status})`);
    const result = await response.json<{ finalMessage?: unknown }>();
    if (typeof result.finalMessage !== "string") throw new Error("Chief of Staff turn was malformed");
    await thread.post(result.finalMessage);
  };
  chat.onDirectMessage(handle);
  chat.onNewMention(handle);
  chat.onSubscribedMessage(handle);
  const runtime = { chat, slack };
  runtimes.set(env as object, runtime);
  return runtime;
}

async function readiness(request: Request, env: Env): Promise<Response> {
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  const requester = await requestingAccountId(env.NANOCODEX_BACKEND, request);
  if (!requester) return json({ error: "unauthorized" }, { status: 401 });
  const config = configurationReadiness(env);
  const owner = config.slack.configured || config.viber.configured
    ? await configuredOwnerId(env)
    : undefined;
  const accountMatch = owner === requester;
  const installations = accountMatch
    ? (await installationMetadataList(env))
      .filter((installation) => installation.accountId === requester)
      .map(({ accountId: _, ...installation }) => installation)
    : [];
  const ready = config.configured && accountMatch && installations.length > 0;
  const viberReady = config.viber.configured && accountMatch;
  const body: Readiness = {
    accountMatch,
    configured: ready || viberReady,
    installations,
    installUrl: config.configured && accountMatch ? "/api/chief-of-staff/slack/install" : null,
    webhookUrl: config.webhookUrl,
    channels: [
      {
        id: "slack",
        availability: ready ? "ready" : "setup_required",
        contract: "first_party",
        detail: ready
          ? `${installations.length} Slack workspace${installations.length === 1 ? "" : "s"} route bot events to account-owned durable agents.`
          : config.configured
            ? accountMatch
              ? "Install the Chief of Staff bot into a Slack workspace."
              : "This deployment is bound to a different Nanocodex account."
            : "The shared Slack app is not configured by the deployment operator.",
      },
      {
        id: "whatsapp",
        availability: "not_enabled",
        contract: "first_party",
        detail: "The current SDK adapter is supported, but this deployment does not claim a configured Meta webhook.",
      },
      {
        id: "imessage",
        availability: "not_enabled",
        contract: "vendor_official",
        detail: "Chat SDK catalogs iMessage through vendor adapters; no first-party iMessage channel is enabled here.",
      },
      {
        id: "viber",
        availability: viberReady ? "ready" : "setup_required",
        contract: "first_party",
        detail: config.viber.configured
          ? accountMatch
            ? "Signed Viber callbacks route each subscriber to an account-owned durable agent."
            : "This deployment is bound to a different Nanocodex account."
          : "Viber bot token, bot identity, or public origin is incomplete.",
        webhookUrl: config.viber.webhookUrl,
      },
    ],
  };
  return json(body);
}

async function installationMetadataList(env: Env): Promise<SlackInstallationMetadata[]> {
  const response = await installationsObject(env).fetch(
    new Request("https://state.internal/slack/installations"),
  );
  if (!response.ok) throw new Error("Slack installation metadata is unavailable");
  const body: unknown = await response.json();
  if (!isRecord(body) || !Array.isArray(body.installations)) {
    throw new Error("Slack installation metadata is malformed");
  }
  return body.installations.filter(validSlackInstallationMetadata);
}

async function installationMetadata(
  env: Env,
  teamId: string,
): Promise<SlackInstallationMetadata | undefined> {
  return (await installationMetadataList(env)).find((installation) => installation.teamId === teamId);
}

async function writeInstallationMetadata(
  env: Env,
  metadata: SlackInstallationMetadata,
  method: "DELETE" | "PUT",
): Promise<void> {
  const response = await installationsObject(env).fetch(new Request(
    "https://state.internal/slack/installations",
    {
      body: JSON.stringify(metadata),
      headers: { "content-type": "application/json" },
      method,
    },
  ));
  if (!response.ok) throw new Error("Slack installation metadata update failed");
  await response.body?.cancel();
}

function installationsObject(env: Env): Fetcher {
  return env.CHIEF_OF_STAFF_STATE.getByName(INSTALLATIONS_OBJECT);
}

function logViberFailure(operation: string, error: unknown): void {
  console.warn({
    type: `chief_of_staff.viber_${operation}`,
    error_kind: error instanceof Error ? error.name : typeof error,
  });
}

function configuredOwnerId(env: Env): Promise<string | undefined> {
  const retained = ownerIds.get(env as object);
  if (retained) return retained;
  const loading = env.NANOCODEX_BACKEND && env.NANOCODEX_API_KEY
    ? new NanocodexManagedGateway(env.NANOCODEX_BACKEND, env.NANOCODEX_API_KEY).ownerId()
    : Promise.resolve(undefined);
  ownerIds.set(env as object, loading);
  return loading;
}

function installationReturn(env: Env, result: string): Response {
  let origin: URL;
  try { origin = new URL(env.CHIEF_OF_STAFF_ACCOUNT_ORIGIN ?? ""); }
  catch { return json({ error: `slack_install_${result}` }, { status: result === "installed" ? 200 : 400 }); }
  if (origin.protocol !== "https:" || origin.pathname !== "/" || origin.search || origin.hash) {
    return json({ error: `slack_install_${result}` }, { status: result === "installed" ? 200 : 400 });
  }
  const target = new URL("/demos/chief-of-staff", origin);
  target.searchParams.set("slack", result);
  return new Response(null, {
    status: 303,
    headers: { "cache-control": "no-store", location: target.href },
  });
}

function messageTeamId(raw: unknown): string | undefined {
  if (!isRecord(raw)) return undefined;
  const value = raw.team_id ?? raw.team;
  return typeof value === "string" && SLACK_TEAM_ID.test(value) ? value : undefined;
}

function slackEventType(payload: SlackWebhookPayload): string | undefined {
  if (!isRecord(payload.raw) || !isRecord(payload.raw.event)) return undefined;
  return typeof payload.raw.event.type === "string" ? payload.raw.event.type : undefined;
}

function methodNotAllowed(methods: readonly string[]): Response {
  return json({ error: "method_not_allowed" }, {
    status: 405,
    headers: { allow: methods.join(", ") },
  });
}

function json(body: unknown, init: ResponseInit = {}): Response {
  return Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init.headers,
    },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
