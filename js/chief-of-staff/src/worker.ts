import { createSlackAdapter } from "@chat-adapter/slack";
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
  slackPayloadIsFenced,
  type Readiness,
} from "./protocol.ts";
import { DurableChatStateAdapter } from "./state-adapter.ts";

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
type StateNamespace = Readonly<{ getByName(name: string): Fetcher }>;

export interface Env {
  CHIEF_OF_STAFF_PUBLIC_ORIGIN?: string;
  CHIEF_OF_STAFF_STATE: StateNamespace;
  NANOCODEX_API_KEY?: string;
  NANOCODEX_BACKEND?: Fetcher;
  SLACK_BOT_TOKEN?: string;
  SLACK_BOT_USER_ID?: string;
  SLACK_SIGNING_SECRET?: string;
  SLACK_TEAM_ID?: string;
}

const chats = new WeakMap<object, Chat>();
const ownerIds = new WeakMap<object, Promise<string | undefined>>();

export default {
  async fetch(request: Request, env: Env, context?: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health" && request.method === "GET") {
      return json({ service: "nanocodex-chief-of-staff", status: "ok" });
    }
    if (url.pathname === "/v1/readiness" && request.method === "GET") {
      return readiness(request, env);
    }
    if (url.pathname === "/webhooks/slack") {
      if (request.method !== "POST") return methodNotAllowed(["POST"]);
      return slackWebhook(request, env, context);
    }
    return json({ error: "not_found" }, { status: 404 });
  },
};

async function slackWebhook(
  request: Request,
  env: Env,
  context?: ExecutionContext,
): Promise<Response> {
  const config = configurationReadiness(env);
  if (!config.configured || !env.SLACK_SIGNING_SECRET || !env.SLACK_TEAM_ID) {
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
  if (!slackPayloadIsFenced(payload, env.SLACK_TEAM_ID)) {
    return json({ error: "workspace_forbidden" }, { status: 403 });
  }
  try {
    const webhookRequest = request as unknown as Parameters<Chat["webhooks"]["slack"]>[0];
    return await chiefChat(env).webhooks.slack(webhookRequest, {
      waitUntil(task) {
        const handled = task.catch((error) => {
          console.warn({
            type: "chief_of_staff.slack_delivery_failed",
            error_kind: error instanceof Error ? error.name : typeof error,
          });
        });
        if (context) context.waitUntil(handled);
      },
    });
  } catch (error) {
    console.warn({
      type: "chief_of_staff.slack_webhook_failed",
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "channel_unavailable" }, { status: 503 });
  }
}

function chiefChat(env: Env): Chat {
  const retained = chats.get(env as object);
  if (retained) return retained;
  const stateObject = env.CHIEF_OF_STAFF_STATE.getByName("chat-sdk:slack");
  const slack = createSlackAdapter({
    agentView: true,
    botToken: env.SLACK_BOT_TOKEN!,
    botUserId: env.SLACK_BOT_USER_ID!,
    logger: undefined,
    signingSecret: undefined,
    webhookVerifier: () => true,
  });
  const bot = new Chat({
    adapters: { slack },
    concurrency: "concurrent",
    dedupeTtlMs: 24 * 60 * 60 * 1_000,
    logger: "warn",
    state: new DurableChatStateAdapter(stateObject),
    userName: "chief-of-staff",
  });
  const handle = async (thread: Thread, message: Message) => {
    await thread.subscribe();
    const ownerId = await configuredOwnerId(env);
    if (!ownerId) throw new Error("Configured Nanocodex account is unavailable");
    const identity = slackMessageIdentity(message.raw, thread.id, thread.isDM, ownerId);
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
  bot.onDirectMessage(handle);
  bot.onNewMention(handle);
  bot.onSubscribedMessage(handle);
  chats.set(env as object, bot);
  return bot;
}

async function readiness(request: Request, env: Env): Promise<Response> {
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  const requester = await requestingAccountId(env.NANOCODEX_BACKEND, request);
  if (!requester) return json({ error: "unauthorized" }, { status: 401 });
  const config = configurationReadiness(env);
  const owner = config.configured ? await configuredOwnerId(env) : undefined;
  const accountMatch = owner === requester;
  const body: Readiness = {
    accountMatch,
    configured: config.configured && accountMatch,
    webhookUrl: config.webhookUrl,
    channels: [
      {
        id: "slack",
        availability: config.configured && accountMatch ? "ready" : "setup_required",
        contract: "first_party",
        detail: config.configured
          ? accountMatch
            ? "Signed Slack events route to account-owned durable agents."
            : "This deployment is bound to a different Nanocodex account."
          : "Worker secrets, workspace identity, or public origin are incomplete.",
      },
      {
        id: "whatsapp",
        availability: "not_enabled",
        contract: "first_party",
        detail: "The current SDK adapter is supported, but this first deployment does not claim a configured Meta webhook.",
      },
      {
        id: "imessage",
        availability: "not_enabled",
        contract: "vendor_official",
        detail: "Chat SDK catalogs iMessage through vendor adapters; no first-party iMessage channel is enabled here.",
      },
    ],
  };
  return json(body);
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
