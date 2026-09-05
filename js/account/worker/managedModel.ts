import { cloudflareEgress } from "nanocodex/cloudflare/egress";

export type ManagedModelEnv = {
  NANOCODEX_BACKEND?: Fetcher;
};

export type ManagedModelAccess = Readonly<{
  binding: Fetcher;
}>;

export type ManagedModelStatus = Readonly<{
  freePromptsRemaining: number | null;
  ready: boolean;
  source: "brokered" | "sponsored" | null;
  voiceEnabled: boolean;
}>;

export type ManagedRealtimeIdentity = Readonly<{
  openAiAlpha: "quicksilver=v2";
  realtimeSessionId: string;
  sessionId: string;
  threadId: string;
}>;

type ManagedHttpOperation = "image_edit" | "image_generation" | "search";

const HTTP_OPERATIONS = Object.freeze({
  image_edit: "https://nanocodex.internal/v1/images/edits",
  image_generation: "https://nanocodex.internal/v1/images/generations",
  search: "https://nanocodex.internal/v1/search",
});

const CREDENTIAL_STATUS_URL = "https://broker.internal/.well-known/nanocodex/model-status";
const SPONSORED_TRIAL_RESET_URL = "https://broker.internal/.well-known/nanocodex/sponsored-trial-reset";

/** Resolves the deployment-owned model boundary without reading a provider credential. */
export function managedModelAccess(
  request: Request,
  env: ManagedModelEnv,
): ManagedModelAccess | undefined {
  if (env.NANOCODEX_BACKEND === undefined) return undefined;
  if (typeof env.NANOCODEX_BACKEND.fetch !== "function") {
    throw new Error("model access requires the private managed Service Binding");
  }
  const cookie = request.headers.get("cookie");
  const backend = env.NANOCODEX_BACKEND;
  const binding = {
    fetch(input: RequestInfo | URL, init?: RequestInit) {
      const scoped = new Request(input, init);
      const headers = new Headers(scoped.headers);
      if (cookie) headers.set("cookie", cookie);
      return backend.fetch(new Request(scoped, { headers }));
    },
  } as Fetcher;
  return Object.freeze({ binding });
}

/** Checks broker policy/credential availability without opening a provider connection. */
export async function managedModelReady(access: ManagedModelAccess): Promise<boolean> {
  return (await managedModelStatus(access)).ready;
}

/** Checks broker readiness and the provider capability needed by Realtime voice. */
export async function managedModelStatus(access: ManagedModelAccess): Promise<ManagedModelStatus> {
  try {
    const response = await access.binding.fetch(new Request(CREDENTIAL_STATUS_URL));
    if (response.status !== 200
      || response.headers.get("cache-control") !== "no-store"
      || !response.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
      await response.body?.cancel();
      return { freePromptsRemaining: null, ready: false, source: null, voiceEnabled: false };
    }
    const encoded = await response.text();
    if (encoded.length > 1_024) {
      return { freePromptsRemaining: null, ready: false, source: null, voiceEnabled: false };
    }
    const value = JSON.parse(encoded) as Record<string, unknown>;
    const ready = value !== null
      && !Array.isArray(value)
      && value.ready === true
      && (value.active === "chatgpt" || value.active === "openai");
    const sponsoredRemaining = Number.isSafeInteger(value.free_prompts_remaining)
      && (value.free_prompts_remaining as number) >= 0
      && (value.free_prompts_remaining as number) <= 3
      ? value.free_prompts_remaining as number
      : null;
    const source = ready && value.source === "sponsored" && sponsoredRemaining !== null
      ? "sponsored"
      : ready && value.source === "user" ? "brokered" : null;
    return {
      freePromptsRemaining: source === "sponsored" ? sponsoredRemaining : null,
      ready: source !== null,
      source,
      voiceEnabled: source === "brokered" && value.active === "chatgpt",
    };
  } catch {
    return { freePromptsRemaining: null, ready: false, source: null, voiceEnabled: false };
  }
}

/** Resets only the current development account's sponsored homepage allowance. */
export function resetManagedSponsoredTrial(access: ManagedModelAccess): Promise<Response> {
  return access.binding.fetch(new Request(SPONSORED_TRIAL_RESET_URL, { method: "POST" }));
}

/** Opens the exact placeholder-only Responses WebSocket through the private broker. */
export function openManagedResponsesWebSocket(
  access: ManagedModelAccess,
  sessionId: string,
  threadId: string = sessionId,
) {
  const endpoint = cloudflareEgress({
    binding: access.binding,
  });
  return endpoint.createWebSocket(endpoint.websocketUrl, sessionId, {
    authorization: "host_managed",
    threadId,
  });
}

/** Sends one exact, placeholder-only tool request through the private broker. */
export function fetchManagedModel(
  access: ManagedModelAccess,
  operation: ManagedHttpOperation,
  body: string,
): Promise<Response> {
  const headers = new Headers({
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
    "content-type": "application/json",
    "user-agent": "nanocodex-web/0.1.0",
  });
  return access.binding.fetch(new Request(HTTP_OPERATIONS[operation], {
    method: "POST",
    headers,
    body,
  }));
}

/** Sends Codex's exact ChatGPT Realtime call through the existing private broker. */
export function fetchManagedRealtimeCall(
  access: ManagedModelAccess,
  identity: ManagedRealtimeIdentity,
  body: string,
  agentId?: string,
): Promise<Response> {
  return access.binding.fetch(new Request("https://nanocodex.internal/v1/realtime/calls", {
    method: "POST",
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "content-type": "application/json",
      "user-agent": "nanocodex/0.1.0",
      "openai-alpha": identity.openAiAlpha,
      "x-session-id": identity.realtimeSessionId,
      "session-id": identity.sessionId,
      "thread-id": identity.threadId,
      ...(agentId === undefined ? {} : { "x-nanocodex-agent-id": agentId }),
    },
    body,
  }));
}

/** Opens Codex's exact Realtime sideband through the existing private broker. */
export function openManagedRealtimeSideband(
  access: ManagedModelAccess,
  callId: string,
  identity: ManagedRealtimeIdentity,
  agentId?: string,
): Promise<Response> {
  return access.binding.fetch(new Request("https://nanocodex.internal/v1/realtime/sideband", {
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      upgrade: "websocket",
      "user-agent": "nanocodex/0.1.0",
      "x-nanocodex-realtime-call-id": callId,
      "openai-alpha": identity.openAiAlpha,
      "x-session-id": identity.realtimeSessionId,
      "session-id": identity.sessionId,
      "thread-id": identity.threadId,
      ...(agentId === undefined ? {} : { "x-nanocodex-agent-id": agentId }),
    },
  }));
}

/** Stable, non-secret quota identity for one browser using a shared deployment credential. */
export function managedModelActorId(request: Request, access: ManagedModelAccess): string {
  return [
    "brokered",
    request.headers.get("cf-connecting-ip") ?? "unknown-ip",
    request.headers.get("user-agent") ?? "unknown-agent",
  ].join(":");
}
