import { DurableObject } from "cloudflare:workers";
import { Agent, type ManagedTurn } from "nanocodex/managed";
import {
  ConnectVerificationError,
  connectConfiguration,
  connectCredentials,
  verifyConnectIdentity,
  type ConnectIdentity,
} from "./connect";
import {
  paymentCredential,
  paymentOptions,
  paymentReceiptHeader,
  paymentServer,
  type PaymentConfiguration,
} from "./payment";
import {
  ASTRA_SETTINGS,
  MACH,
  paymentAmount,
  publicTrialState,
  reservePrompt,
  TEMPO_CHAIN_ID,
  type TrialState,
} from "./policy";

const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const ADDRESS = /^0x[0-9a-f]{40}$/;
const API_KEY = /^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/;
const REQUEST_KEY = /^[A-Za-z0-9._:-]{1,128}$/;
const MAX_PROMPT_BYTES = 32 * 1024;
const MAX_BODY_BYTES = MAX_PROMPT_BYTES + 1_024;

export type Env = {
  ASSETS: Fetcher;
  TRIALS: DurableObjectNamespace<AstraTrial>;
  ENVIRONMENT?: string;
  NANOCODEX_ASTRA_MACH_RECIPIENT?: string;
  NANOCODEX_ASTRA_MANAGED_API_KEY?: string;
  NANOCODEX_ASTRA_MPP_SECRET?: string;
  NANOCODEX_CONNECT_API_URL?: string;
  NANOCODEX_CONNECT_APP_ID?: string;
  NANOCODEX_CONNECT_APP_ORIGIN?: string;
  NANOCODEX_CONNECT_DIALOG_URL?: string;
  NANOCODEX_MANAGED_URL?: string;
  TEMPO_MPP_API_KEY?: string;
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/api/")) {
      return secureAsset(await env.ASSETS.fetch(request), connectConfiguration(env));
    }
    const connect = connectConfiguration(env);
    if (!connect) return problem(503, "connect_not_configured");
    if (url.origin !== connect.appOrigin) return problem(421, "wrong_app_origin");
    if (request.method !== "GET" && request.headers.get("origin") !== url.origin) {
      return problem(403, "forbidden_origin");
    }

    try {
      if (request.method === "GET" && url.pathname === "/api/config") {
        const trial = trialConfiguration(env);
        return json({
          amount: paymentAmount(env.ENVIRONMENT),
          chain_id: TEMPO_CHAIN_ID,
          connect: {
            api_url: connect.apiUrl,
            app_id: connect.appId,
            app_origin: connect.appOrigin,
            dialog_url: connect.dialogUrl,
          },
          currency: MACH,
          payment_enabled: Boolean(trial),
          recipient: trial?.payment.recipient,
        });
      }

      if (!["/api/session", "/api/trial", "/api/prompt"].includes(url.pathname)) {
        return problem(404, "not_found");
      }
      if (request.method === "POST" && url.pathname === "/api/prompt"
        && request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase()
          !== "application/json") return problem(415, "json_required");
      const credentials = connectCredentials(request);
      if (!credentials) return problem(401, "connect_session_required");
      const identity = await verifyConnectIdentity(credentials, connect);
      if (request.method === "GET" && url.pathname === "/api/session") {
        return json({
          account_address: identity.accountAddress,
          authenticated: true,
          expires_at: identity.expiresAt,
        });
      }
      const trial = env.TRIALS.getByName(identity.accountId);
      if (request.method === "GET" && url.pathname === "/api/trial") {
        return trial.fetch(internalRequest("/status", identity));
      }
      if (request.method === "POST" && url.pathname === "/api/prompt") {
        return trial.fetch(internalRequest("/prompt", identity, request));
      }
      return problem(405, "method_not_allowed");
    } catch (error) {
      if (error instanceof ConnectVerificationError) return problem(error.status, error.message);
      console.error({ type: "astra_trial.request_failed", error: errorName(error) });
      return problem(500, "internal_error");
    }
  },
} satisfies ExportedHandler<Env>;

export class AstraTrial extends DurableObject<Env> {
  #observedTurns = new Set<string>();
  #tail: Promise<void> = Promise.resolve();

  fetch(request: Request): Promise<Response> {
    return this.#exclusive(async () => this.#dispatch(request));
  }

  async #exclusive<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.#tail;
    let release!: () => void;
    this.#tail = new Promise<void>((resolve) => { release = resolve; });
    await previous;
    try {
      return await operation();
    } finally {
      release();
    }
  }

  async #dispatch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const identity = internalIdentity(request);
    if (!identity) return problem(401, "invalid_internal_identity");
    if (request.method === "GET" && url.pathname === "/status") {
      let state = await this.#state();
      if (state?.phase === "running") state = await this.#resume(state);
      return json(publicTrialState(state));
    }
    if (request.method === "POST" && url.pathname === "/prompt") {
      return this.#prompt(request, identity);
    }
    return problem(404, "not_found");
  }

  async #prompt(request: Request, identity: InternalIdentity): Promise<Response> {
    const configuration = trialConfiguration(this.env);
    if (!configuration) return problem(503, "astra_trial_not_configured");
    const body = await boundedJson(request, MAX_BODY_BYTES);
    const prompt = body && typeof body === "object" && !Array.isArray(body)
      && Object.keys(body).length === 1 && typeof (body as { prompt?: unknown }).prompt === "string"
      ? (body as { prompt: string }).prompt.trim()
      : "";
    const requestKey = request.headers.get("idempotency-key");
    if (!prompt || new TextEncoder().encode(prompt).byteLength > MAX_PROMPT_BYTES
      || !requestKey || !REQUEST_KEY.test(requestKey)) {
      return problem(400, "invalid_prompt");
    }
    const promptHash = await digest(prompt);
    let state = await this.#state();
    const reserved = reservePrompt(state, promptHash, requestKey);
    if (reserved === "conflict") return problem(409, "astra_trial_already_claimed");
    if (state?.phase === "completed" || state?.phase === "failed") {
      return json(publicTrialState(state));
    }
    if (state?.phase === "running") return json(publicTrialState(state), 202);
    if (state?.phase === "payment_pending") {
      return problem(202, "payment_outcome_pending", { "retry-after": "10" });
    }

    let receiptHeader: string | undefined;
    if (state?.phase !== "paid") {
      const payment = paymentServer(this.ctx.storage, configuration.payment);
      const options = paymentOptions(configuration.payment, identity.accountId, promptHash);
      const credential = paymentCredential(request);
      if (!credential) {
        const result = await payment.charge(options)(request);
        return result.status === 402 ? result.challenge : problem(500, "payment_protocol_error");
      }
      const { description: _description, scope, ...expectedRequest } = options;
      let validation;
      try {
        validation = await payment.validateCredential(credential, {
          request: expectedRequest,
          scope,
        });
      } catch {
        const retry = await payment.charge(options)(request);
        return retry.status === 402 ? retry.challenge : problem(402, "payment_rejected");
      }
      const sender = validationSender(validation.details);
      if (!sender || sender !== identity.accountAddress) return problem(403, "payment_account_mismatch");

      state = { ...reserved, prompt };
      await this.ctx.storage.put("trial", state);
      let receipt;
      try {
        receipt = await payment.broadcastCredential(credential, {
          request: expectedRequest,
          scope,
        });
      } catch (error) {
        console.error({ type: "astra_trial.payment_ambiguous", error: errorName(error) });
        return problem(503, "payment_outcome_pending", { "retry-after": "10" });
      }
      receiptHeader = paymentReceiptHeader(receipt);
      state = {
        ...state,
        paymentReference: receipt.reference,
        phase: "paid",
        updatedAt: Date.now(),
      };
      await this.ctx.storage.put("trial", state);
    }

    try {
      const managed = configuration.managed;
      const agent = state.agentId
        ? await Agent.get(state.agentId, managed)
        : await Agent.create({ ...managed, settings: ASTRA_SETTINGS });
      if (!state.agentId) {
        state = { ...state, agentId: agent.id, updatedAt: Date.now() };
        await this.ctx.storage.put("trial", state);
      }
      const settings = await agent.settings.read();
      if (!astraSettings(settings)) throw new Error("managed Astra policy mismatch");
      const turn = agent.turn.prompt({
        id: "astra-trial",
        idempotencyKey: `astra-trial:${requestKey}`,
        input: prompt,
      });
      const turnId = await turn.accepted();
      state = { ...state, phase: "running", turnId, updatedAt: Date.now() };
      await this.ctx.storage.put("trial", state);
      this.#watchTurn(turn, state);
      const response = json(publicTrialState(state), 202);
      return receiptHeader
        ? new Response(response.body, {
            status: response.status,
            headers: { ...Object.fromEntries(response.headers), "payment-receipt": receiptHeader },
          })
        : response;
    } catch (error) {
      console.error({ type: "astra_trial.agent_unavailable", error: errorName(error) });
      return problem(503, "astra_agent_unavailable", { "retry-after": "2" });
    }
  }

  #state(): Promise<TrialState | undefined> {
    return this.ctx.storage.get<TrialState>("trial");
  }

  async #resume(state: TrialState): Promise<TrialState> {
    if (!state.agentId || !state.turnId || !state.requestKey || !state.prompt) return state;
    const configuration = trialConfiguration(this.env);
    if (!configuration) return state;
    try {
      const agent = await Agent.get(state.agentId, configuration.managed);
      const turn = agent.turn.prompt({
        id: "astra-trial",
        idempotencyKey: `astra-trial:${state.requestKey}`,
        input: state.prompt,
      });
      const view = await turn.state();
      if (view.terminal?.type === "turn_completed") {
        const completed = {
          ...state,
          finalMessage: view.terminal.final_message,
          phase: "completed",
          updatedAt: Date.now(),
        } satisfies TrialState;
        await this.ctx.storage.put("trial", completed);
        return completed;
      }
      if (view.state === "failed" || view.state === "cancelled") {
        const failed = failedTrial(state);
        await this.ctx.storage.put("trial", failed);
        return failed;
      }
      this.#watchTurn(turn, state);
    } catch (error) {
      console.error({ type: "astra_trial.resume_failed", error: errorName(error) });
    }
    return state;
  }

  #watchTurn(turn: ManagedTurn, state: TrialState): void {
    if (!state.turnId || this.#observedTurns.has(state.turnId)) return;
    this.#observedTurns.add(state.turnId);
    const completion = turn.result().then(async (result) => {
      await this.ctx.storage.put("trial", {
        ...state,
        finalMessage: result.finalMessage,
        phase: "completed",
        updatedAt: Date.now(),
      } satisfies TrialState);
    }).catch(async (error) => {
      console.error({ type: "astra_trial.turn_failed", error: errorName(error) });
      const view = await turn.state().catch(() => undefined);
      if (view?.terminal?.type === "turn_completed") {
        await this.ctx.storage.put("trial", {
          ...state,
          finalMessage: view.terminal.final_message,
          phase: "completed",
          updatedAt: Date.now(),
        } satisfies TrialState);
      } else if (view?.state === "failed" || view?.state === "cancelled") {
        await this.ctx.storage.put("trial", failedTrial(state));
      }
    }).finally(() => {
      this.#observedTurns.delete(state.turnId!);
    });
    this.ctx.waitUntil(completion);
  }
}

type InternalIdentity = Readonly<{ accountAddress: `0x${string}`; accountId: string }>;

type TrialConfiguration = Readonly<{
  managed: Readonly<{ apiKey: string; baseUrl: string }>;
  payment: PaymentConfiguration;
}>;

function trialConfiguration(env: Env): TrialConfiguration | undefined {
  const amount = paymentAmount(env.ENVIRONMENT);
  const apiKey = env.NANOCODEX_ASTRA_MANAGED_API_KEY?.trim();
  const baseUrl = serviceUrl(env.NANOCODEX_MANAGED_URL?.trim());
  const recipient = env.NANOCODEX_ASTRA_MACH_RECIPIENT?.trim().toLowerCase();
  const secret = env.NANOCODEX_ASTRA_MPP_SECRET?.trim();
  const tempoApiKey = env.TEMPO_MPP_API_KEY?.trim();
  if (!apiKey || !API_KEY.test(apiKey) || !baseUrl || !recipient || !ADDRESS.test(recipient)
    || !secret || secret.length < 32 || (amount !== "0" && !tempoApiKey)) return undefined;
  return {
    managed: { apiKey, baseUrl },
    payment: {
      amount,
      recipient: recipient as `0x${string}`,
      secret,
      ...(tempoApiKey ? { tempoApiKey } : {}),
    },
  };
}

function internalRequest(path: string, identity: ConnectIdentity, original?: Request): Request {
  const headers = new Headers({
    "x-trial-account-address": identity.accountAddress,
    "x-trial-account-id": identity.accountId,
  });
  if (original) {
    for (const name of ["content-type", "idempotency-key", "payment-authorization"]) {
      const value = original.headers.get(name);
      if (value) headers.set(name, value);
    }
  }
  return new Request(`https://trial.internal${path}`, {
    method: original?.method ?? "GET",
    headers,
    body: original?.body,
  });
}

function internalIdentity(request: Request): InternalIdentity | undefined {
  const accountId = request.headers.get("x-trial-account-id");
  const accountAddress = request.headers.get("x-trial-account-address")?.toLowerCase();
  return accountId && ACCOUNT_ID.test(accountId) && accountAddress && ADDRESS.test(accountAddress)
    ? { accountAddress: accountAddress as `0x${string}`, accountId }
    : undefined;
}

function validationSender(value: unknown): string | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const sender = (value as { sender?: unknown }).sender;
  return typeof sender === "string" && ADDRESS.test(sender.toLowerCase())
    ? sender.toLowerCase()
    : undefined;
}

function astraSettings(value: unknown): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const settings = value as Record<string, unknown>;
  return settings.model === ASTRA_SETTINGS.model
    && settings.thinking === ASTRA_SETTINGS.thinking
    && settings.reasoningMode === ASTRA_SETTINGS.reasoningMode
    && settings.fastMode === ASTRA_SETTINGS.fastMode;
}

function serviceUrl(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
    if (url.username || url.password || url.hash
      || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))) return undefined;
    return url.toString();
  } catch {
    return undefined;
  }
}

async function digest(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function boundedJson(request: Request, maximumBytes: number): Promise<unknown> {
  const reader = request.body?.getReader();
  if (!reader) return undefined;
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      length += next.value.byteLength;
      if (length > maximumBytes) {
        await reader.cancel();
        return undefined;
      }
      chunks.push(next.value);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    return undefined;
  }
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}

function failedTrial(state: TrialState): TrialState {
  return {
    ...state,
    error: "Astra was unable to complete this prompt.",
    phase: "failed",
    updatedAt: Date.now(),
  };
}

function secureAsset(response: Response, connect: ReturnType<typeof connectConfiguration>): Response {
  const headers = new Headers(response.headers);
  const connectOrigin = connect ? new URL(connect.apiUrl).origin : "";
  headers.set("content-security-policy", [
    "default-src 'self'",
    "base-uri 'none'",
    `connect-src 'self' ${connectOrigin}`.trim(),
    "font-src 'self'",
    "frame-src 'none'",
    "frame-ancestors 'none'",
    "form-action 'self'",
    "img-src 'self' data:",
    "object-src 'none'",
    "script-src 'self'",
    "style-src 'self'",
  ].join("; "));
  headers.set("permissions-policy", "camera=(), geolocation=(), microphone=()");
  headers.set("referrer-policy", "no-referrer");
  headers.set("x-content-type-options", "nosniff");
  headers.set("x-frame-options", "DENY");
  return new Response(response.body, { status: response.status, headers });
}

function problem(status: number, error: string, headers?: HeadersInit): Response {
  return json({ error }, status, headers);
}

function json(body: unknown, status = 200, headers?: HeadersInit): Response {
  return Response.json(body, {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...Object.fromEntries(new Headers(headers)),
    },
  });
}
