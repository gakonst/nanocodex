import { DurableObject } from "cloudflare:workers";
import { Agent } from "nanocodex/cloudflare";
import { authenticate } from "./auth";

type ChatGptImport = Readonly<{
  access_token: string; refresh_token: string; account_id: string;
  expires_at: number; fedramp: boolean;
}>;
type Egress = Fetcher & {
  putCredential(owner: string, provider: string, value: string): Promise<void>;
  putChatGptCredential(owner: string, value: ChatGptImport): Promise<void>;
};
type Env = { SESSIONS: DurableObjectNamespace<Session>; EGRESS: Egress; AUTH_API_KEY_HASHES: string; RESPONSES_TRANSPORT?: "websocket" };
const AGENT_PATH = /^\/v1\/agents\/([0-9a-f-]{36})(?:\/(turns|turns\/([0-9a-f-]{36})|events))?$/;
const OWNER_HEADER = "x-managed2-owner";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const create = request.method === "POST" && url.pathname === "/v1/agents";
    const credential = request.method === "PUT" && (
      url.pathname === "/v1/credentials/openai" || url.pathname === "/v1/credentials/chatgpt"
    );
    const match = AGENT_PATH.exec(url.pathname);
    if (!create && !credential && !match) return reply(404, { error: "not_found" });
    const events = match?.[2] === "events";
    const authStart = performance.now();
    const principal = await authenticate(request, env.AUTH_API_KEY_HASHES);
    const authTiming = `auth;dur=${(performance.now() - authStart).toFixed(1)}`;
    if (!principal) {
      const denied = reply(401, { error: "unauthorized" });
      denied.headers.set("server-timing", authTiming);
      return denied;
    }

    if (credential) {
      const body = await jsonBody(request);
      if (url.pathname === "/v1/credentials/chatgpt") {
        if (!body || typeof body.access_token !== "string" || !body.access_token
          || typeof body.refresh_token !== "string" || !body.refresh_token
          || typeof body.account_id !== "string" || !body.account_id
          || typeof body.expires_at !== "number" || !Number.isSafeInteger(body.expires_at)
          || typeof body.fedramp !== "boolean") return reply(400, { error: "invalid_credential" });
        await env.EGRESS.putChatGptCredential(principal.sub, body as ChatGptImport);
      } else {
        if (!body || typeof body.value !== "string" || !body.value) {
          return reply(400, { error: "invalid_credential" });
        }
        await env.EGRESS.putCredential(principal.sub, "openai", body.value);
      }
      return new Response(null, { status: 204, headers: { "server-timing": authTiming } });
    }
    if (create) {
      // The optional first turn and agent initialization share one Session RPC.
      // Supplying an Idempotency-Key makes the agent address stable on retry.
      const hasBody = request.body !== null;
      const body = hasBody ? await jsonBody(request) : undefined;
      if (hasBody && (!body || typeof body.input !== "string" || !body.input.trim())) {
        return reply(400, { error: "invalid_input" });
      }
      const key = request.headers.get("idempotency-key");
      if (key !== null && !/^[0-9a-f-]{36}$/.test(key)) return reply(400, { error: "invalid_idempotency_key" });
      const id = key ?? crypto.randomUUID();
      const stub = env.SESSIONS.getByName(`${principal.sub}:${id}`);
      const headers = new Headers({ [OWNER_HEADER]: principal.sub, "x-managed2-agent": id });
      if (body) headers.set("content-type", "application/json");
      const response = await timedSessionFetch(stub, "https://session.internal/init", {
        method: "POST", headers, ...(body ? { body: JSON.stringify({ input: body.input, turn_id: id }) } : {}),
      }, authTiming);
      if (!response.ok) return response;
      const created = reply(body ? 202 : 201, body
        ? { agent_id: id, ...await response.json<{ turn_id: string; state: string }>() }
        : { agent_id: id });
      created.headers.set("server-timing", response.headers.get("server-timing") ?? "");
      return created;
    }
    const id = match![1]!;
    const stub = env.SESSIONS.getByName(`${principal.sub}:${id}`);
    const headers = new Headers({ [OWNER_HEADER]: principal.sub, "x-managed2-agent": id });
    if (events) {
      if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
      headers.set("upgrade", request.headers.get("upgrade") ?? "");
      return stub.fetch(`https://session.internal/events${url.search}`, { headers });
    }
    if (match![3]) {
      if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
      return timedSessionFetch(stub, `https://session.internal/turns/${match![3]}`, { headers }, authTiming);
    }
    if (match![2] === "turns") {
      if (request.method !== "POST") return reply(405, { error: "method_not_allowed" });
      const body = await jsonBody(request);
      if (!body || typeof body.input !== "string" || !body.input.trim()) {
        return reply(400, { error: "invalid_input" });
      }
      const key = request.headers.get("idempotency-key") ?? crypto.randomUUID();
      if (!/^[0-9a-f-]{36}$/.test(key)) return reply(400, { error: "invalid_idempotency_key" });
      headers.set("idempotency-key", key);
      headers.set("content-type", "application/json");
      return timedSessionFetch(stub, "https://session.internal/turns", {
        method: "POST", headers, body: JSON.stringify({ input: body.input }),
      }, authTiming);
    }
    if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
    return timedSessionFetch(stub, "https://session.internal/state", { headers }, authTiming);
  },
} satisfies ExportedHandler<Env>;

export class Session extends DurableObject<Env> {
  #agent?: Promise<Agent.Agent>;
  #running = new Set<string>();
  #admissions = new Map<string, { input: string; outcome: Promise<{ status: number; body: string; headers: [string, string][] }> }>();
  #awaitingFirstModel = new Map<string, number>();

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS session_meta (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), owner TEXT NOT NULL, agent_id TEXT NOT NULL
    )`);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS turns (
      id TEXT PRIMARY KEY, input TEXT NOT NULL, state TEXT NOT NULL,
      message TEXT, error TEXT
    )`);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const owner = request.headers.get(OWNER_HEADER);
    const agentId = request.headers.get("x-managed2-agent");
    if (!owner || !agentId) return reply(403, { error: "forbidden" });
    const row = this.ctx.storage.sql.exec<{ owner: string; agent_id: string }>(
      "SELECT owner, agent_id FROM session_meta WHERE singleton = 1",
    ).toArray()[0];
    if (url.pathname === "/init" && request.method === "POST") {
      if (row && (row.owner !== owner || row.agent_id !== agentId)) return reply(403, { error: "forbidden" });
      if (!row) this.ctx.storage.sql.exec(
        "INSERT INTO session_meta (singleton, owner, agent_id) VALUES (1, ?, ?)", owner, agentId,
      );
      if (request.body === null) return new Response(null, { status: 204 });
      const { input, turn_id: turnId } = await request.json<{ input: string; turn_id: string }>();
      return this.#admitTurn(owner, turnId, input);
    }
    if (!row || row.owner !== owner || row.agent_id !== agentId) return reply(404, { error: "not_found" });
    if (url.pathname === "/state" && request.method === "GET") {
      return reply(200, { agent_id: agentId });
    }
    if (url.pathname === "/events" && request.method === "GET") {
      return (await this.#ready(owner)).events.connect(request);
    }
    if (url.pathname === "/turns" && request.method === "POST") {
      const key = request.headers.get("idempotency-key")!;
      const body = await request.json<{ input: string }>();
      return this.#admitTurn(owner, key, body.input);
    }
    const turnId = /^\/turns\/([0-9a-f-]{36})$/.exec(url.pathname)?.[1];
    if (turnId && request.method === "GET") {
      const turn = this.#turn(turnId);
      return turn ? reply(200, { turn_id: turnId, state: turn.state,
        ...(turn.message === null ? {} : { message: turn.message }),
        ...(turn.error === null ? {} : { error: turn.error }) })
        : reply(404, { error: "not_found" });
    }
    return reply(404, { error: "not_found" });
  }

  #admitTurn(owner: string, turnId: string, input: string): Promise<Response> {
    // DO requests can interleave while initialization awaits. Share the entire
    // acceptance result, not just the SQLite insert, with concurrent retries.
    const pending = this.#admissions.get(turnId);
    if (pending && pending.input !== input) return Promise.resolve(reply(409, { error: "idempotency_conflict" }));
    const outcome = pending?.outcome ?? this.#admitTurnOnce(owner, turnId, input).then(async response => ({
      status: response.status,
      body: await response.text(),
      headers: [...response.headers] as [string, string][],
    }));
    if (!pending) {
      this.#admissions.set(turnId, { input, outcome });
      void outcome.then(
        () => { if (this.#admissions.get(turnId)?.outcome === outcome) this.#admissions.delete(turnId); },
        () => { if (this.#admissions.get(turnId)?.outcome === outcome) this.#admissions.delete(turnId); },
      );
    }
    // A Response body is single-use. Every concurrent DO fetch needs a fresh
    // response even though all wait for the same durable acceptance outcome.
    return outcome.then(({ status, body, headers }) => new Response(body, { status, headers }));
  }

  async #admitTurnOnce(owner: string, turnId: string, input: string): Promise<Response> {
    const existing = this.#turn(turnId);
    if (existing) {
      if (existing.input !== input) return reply(409, { error: "idempotency_conflict" });
      if (existing.state === "pending") {
        await this.ctx.storage.setAlarm(Date.now() + 1_000);
        return reply(503, { error: "admission_uncertain", turn_id: turnId });
      }
      return reply(202, { turn_id: turnId, state: existing.state });
    }
    // Agent initialization must finish before the turn is persisted.
    const agentInitStart = performance.now();
    try { await this.#ready(owner); }
    catch {
      const unavailable = reply(503, { error: "model_unavailable" });
      unavailable.headers.set("server-timing", `agent_init;dur=${(performance.now() - agentInitStart).toFixed(1)}`);
      return unavailable;
    }
    const initMs = performance.now() - agentInitStart;
    this.ctx.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES (?, ?, 'pending')", turnId, input);
    const admissionStart = performance.now();
    try {
      await this.#dispatch(turnId, input, owner);
      const accepted = reply(202, { turn_id: turnId, state: "accepted" });
      accepted.headers.set("server-timing", `agent_init;dur=${initMs.toFixed(1)}, admission;dur=${(performance.now() - admissionStart).toFixed(1)}`);
      return accepted;
    } catch (error) {
      console.warn("managed2 admission unavailable", error instanceof Error ? error.name : "error");
      // A command may have been admitted before its reply failed. Retain its
      // stable ID and input so alarm/retry can reconcile rather than duplicate.
      this.ctx.storage.setAlarm(Date.now() + 1_000);
      return reply(503, { error: "admission_uncertain", turn_id: turnId });
    }
  }

  async alarm(): Promise<void> {
    const row = this.ctx.storage.sql.exec<{ owner: string }>(
      "SELECT owner FROM session_meta WHERE singleton = 1",
    ).toArray()[0];
    if (!row) return;
    const turns = this.ctx.storage.sql.exec<{ id: string; input: string }>(
      "SELECT id, input FROM turns WHERE state IN ('pending', 'accepted')",
    ).toArray();
    for (const turn of turns) {
      if (this.#running.has(turn.id)) continue;
      try { await this.#dispatch(turn.id, turn.input, row.owner); }
      catch { /* A later alarm retries with the same Rust durable turn ID. */ }
    }
    if (this.ctx.storage.sql.exec<{ n: number }>(
      "SELECT COUNT(*) AS n FROM turns WHERE state IN ('pending', 'accepted')",
    ).toArray()[0]!.n > 0) await this.ctx.storage.setAlarm(Date.now() + 10_000);
  }

  #turn(id: string) {
    return this.ctx.storage.sql.exec<{ id: string; input: string; state: string; message: string | null; error: string | null }>(
      "SELECT id, input, state, message, error FROM turns WHERE id = ?", id,
    ).toArray()[0];
  }

  #ready(owner: string): Promise<Agent.Agent> {
    if (this.#agent) return this.#agent;
    const options = { tools: [], instructions: "You are a concise assistant." };
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalConfiguration"), { value: {
      model: "gpt-6-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false,
    } });
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), { value: {
      ...(this.env.RESPONSES_TRANSPORT === "websocket"
        ? { waitForPreconnect: true }
        : { inferenceForSession: () => ({ model: "gpt-6-sol", thinking: "low" }) }),
      subagentsEnabled: false,
    } });
    return this.#agent = Agent.create({ ctx: this.ctx, env: { NANOCODEX: {
      fetch: (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
        const source = new Request(input, init);
        if (source.url !== "https://nanocodex.internal/v1/responses") {
          throw new Error("model transport requested an unexpected endpoint");
        }
        const headers = new Headers(source.headers);
        headers.set(OWNER_HEADER, owner);
        const began = performance.now();
        const first = this.#awaitingFirstModel.size === 1 ? this.#awaitingFirstModel.entries().next().value : undefined;
        if (first) this.#awaitingFirstModel.delete(first[0]);
        const promptToEgressMs = first ? +(began - first[1]).toFixed(1) : undefined;
        return this.env.EGRESS.fetch("https://api.openai.com/v1/responses", {
          method: source.method, headers, body: source.body, signal: source.signal,
          redirect: "manual",
        }).then(response => {
          const egressTiming = response.headers.get("server-timing") ?? "";
          const marker = /(?:^|, )egress_route;desc="(openai_api|chatgpt_subscription)"(?:,|$)/.exec(egressTiming)?.[1];
          const dispatch = /(?:^|, )egress_dispatch;dur=([0-9.]+)(?:,|$)/.exec(egressTiming)?.[1];
          console.info({ event: "managed2.model_route",
            ...(promptToEgressMs === undefined ? {} : { prompt_to_egress_ms: promptToEgressMs }),
            egress_headers_ms: +(performance.now() - began).toFixed(1),
            status: response.status,
            route: marker === "openai_api" || marker === "chatgpt_subscription" ? marker : "unknown",
            ...(dispatch === undefined ? {} : { egress_dispatch_ms: Number(dispatch) }),
            egress_timing: egressTiming,
          });
          return response;
        });
      },
    } } }, options)
      .catch(error => { this.#agent = undefined; throw error; });
  }

  async #dispatch(id: string, input: string, owner: string): Promise<void> {
    if (this.#running.has(id)) return;
    const agent = await this.#ready(owner);
    this.#awaitingFirstModel.set(id, performance.now());
    const turn = agent.turn.prompt({ id, input });
    try {
      await turn.accepted();
      this.ctx.storage.sql.exec("UPDATE turns SET state = 'accepted' WHERE id = ?", id);
      this.#running.add(id);
      await this.ctx.storage.setAlarm(Date.now() + 10_000);
      this.ctx.waitUntil((async () => {
        let result: Awaited<ReturnType<typeof turn.result>> | undefined;
        try {
          result = await turn.result();
          this.ctx.storage.sql.exec("UPDATE turns SET state = 'completed', message = ? WHERE id = ?", result.finalMessage, id);
        } catch (error) {
          this.ctx.storage.sql.exec("UPDATE turns SET state = 'failed', error = ? WHERE id = ?",
            error instanceof Error ? error.message : String(error), id);
        } finally {
          result?.dispose();
          turn.dispose();
          this.#running.delete(id);
          this.#awaitingFirstModel.delete(id);
        }
      })());
    } catch (error) {
      turn.dispose();
      this.#awaitingFirstModel.delete(id);
      throw error;
    }
  }
}

async function jsonBody(request: Request): Promise<Record<string, unknown> | undefined> {
  const text = await request.text();
  try {
    const body: unknown = JSON.parse(text);
    return body && typeof body === "object" && !Array.isArray(body) ? body as Record<string, unknown> : undefined;
  } catch { return undefined; }
}
function reply(status: number, body: unknown): Response {
  return Response.json(body, { status, headers: { "cache-control": "no-store" } });
}

async function timedSessionFetch(stub: DurableObjectStub, input: RequestInfo, init: RequestInit, authTiming: string): Promise<Response> {
  const start = performance.now();
  const response = await stub.fetch(input, init);
  return withSessionTiming(response, `${authTiming}, session;dur=${(performance.now() - start).toFixed(1)}`);
}
function withSessionTiming(response: Response, timing: string): Response {
  if (response.status === 101) return response;
  const forwarded = new Response(response.body, response);
  forwarded.headers.set("server-timing", [response.headers.get("server-timing"), timing].filter(Boolean).join(", "));
  return forwarded;
}
