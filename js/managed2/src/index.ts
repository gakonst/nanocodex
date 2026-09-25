import { DurableObject } from "cloudflare:workers";
import { Agent } from "nanocodex/cloudflare";
import type { NamedTool } from "nanocodex";
import { authenticate } from "./auth";
import { ToolTiming } from "./toolTiming";
import { managedWeb } from "./web";

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

// The first tool is intentionally independent of account services or a Hand.
// Its observed turn includes a real model → tool → model continuation.
const currentTime: NamedTool = {
  name: "current_time",
  description: "Get the current UTC date and time. Use when the answer depends on the present time.",
  parameters: { type: "object", properties: {}, additionalProperties: false },
  supportsParallelToolCalls: true,
  handler: () => ({ utc: new Date().toISOString() }),
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const requestStart = performance.now();
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
    const authMs = performance.now() - authStart;
    const authTiming = `auth;dur=${authMs.toFixed(1)}`;
    if (!principal) {
      const denied = reply(401, { error: "unauthorized" });
      denied.headers.set("server-timing", authTiming);
      return denied;
    }

    if (credential) {
      const parseStart = performance.now();
      const body = await jsonBody(request);
      const parseMs = performance.now() - parseStart;
      const storeStart = performance.now();
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
      return new Response(null, { status: 204, headers: { "server-timing": `${authTiming}, body_parse;dur=${parseMs.toFixed(1)}, credential_store;dur=${(performance.now() - storeStart).toFixed(1)}, api_total;dur=${(performance.now() - requestStart).toFixed(1)}` } });
    }
    if (create) {
      // The optional first turn and agent initialization share one Session RPC.
      // Supplying an Idempotency-Key makes the agent address stable on retry.
      const hasBody = request.body !== null;
      const parseStart = performance.now();
      const body = hasBody ? await jsonBody(request) : undefined;
      const bodyTiming = `body_parse;dur=${(performance.now() - parseStart).toFixed(1)}`;
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
      }, `${authTiming}, ${bodyTiming}`, requestStart);
      if (!response.ok) return response;
      const created = reply(body ? 202 : 201, body
        ? { agent_id: id, ...await response.json<{ turn_id: string; state: string }>() }
        : { agent_id: id });
      created.headers.set("server-timing", response.headers.get("server-timing") ?? "");
      const trace = response.headers.get("x-managed2-trace-id");
      if (trace) created.headers.set("x-managed2-trace-id", trace);
      return created;
    }
    const id = match![1]!;
    const stub = env.SESSIONS.getByName(`${principal.sub}:${id}`);
    const headers = new Headers({ [OWNER_HEADER]: principal.sub, "x-managed2-agent": id });
    if (events) {
      if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
      headers.set("upgrade", request.headers.get("upgrade") ?? "");
      const began = performance.now();
      let status: number | null = null;
      try {
        const response = await stub.fetch(`https://session.internal/events${url.search}`, { headers });
        status = response.status;
        return response; // 101 upgrade responses cannot be wrapped with Server-Timing.
      } finally {
        console.info({ event: "managed2.events_connect", status,
          auth_ms: +authMs.toFixed(1),
          session_ms: +(performance.now() - began).toFixed(1),
          total_ms: +(performance.now() - requestStart).toFixed(1) });
      }
    }
    if (match![3]) {
      if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
      return timedSessionFetch(stub, `https://session.internal/turns/${match![3]}`, { headers }, authTiming, requestStart);
    }
    if (match![2] === "turns") {
      if (request.method !== "POST") return reply(405, { error: "method_not_allowed" });
      const parseStart = performance.now();
      const body = await jsonBody(request);
      const bodyTiming = `body_parse;dur=${(performance.now() - parseStart).toFixed(1)}`;
      if (!body || typeof body.input !== "string" || !body.input.trim()) {
        return reply(400, { error: "invalid_input" });
      }
      const key = request.headers.get("idempotency-key") ?? crypto.randomUUID();
      if (!/^[0-9a-f-]{36}$/.test(key)) return reply(400, { error: "invalid_idempotency_key" });
      headers.set("idempotency-key", key);
      headers.set("content-type", "application/json");
      return timedSessionFetch(stub, "https://session.internal/turns", {
        method: "POST", headers, body: JSON.stringify({ input: body.input }),
      }, `${authTiming}, ${bodyTiming}`, requestStart);
    }
    if (request.method !== "GET") return reply(405, { error: "method_not_allowed" });
    return timedSessionFetch(stub, "https://session.internal/state", { headers }, authTiming, requestStart);
  },
} satisfies ExportedHandler<Env>;

export class Session extends DurableObject<Env> {
  #agent?: Promise<Agent.Agent>;
  #running = new Set<string>();
  #admissions = new Map<string, { input: string; outcome: Promise<{ status: number; body: string; headers: [string, string][] }> }>();
  #activeTraces = new Map<string, string>();
  #eventTurns = new Map<string, string>();
  #constructorMs: number;
  #toolTiming: ToolTiming;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    const constructorStart = performance.now();
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS session_meta (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), owner TEXT NOT NULL, agent_id TEXT NOT NULL
    )`);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS turns (
      id TEXT PRIMARY KEY, input TEXT NOT NULL, state TEXT NOT NULL,
      message TEXT, error TEXT
    )`);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS turn_timing (
      id TEXT PRIMARY KEY, trace_id TEXT NOT NULL, started_at INTEGER NOT NULL,
      agent_init_ms REAL, accepted_ms INTEGER, model_send_ms INTEGER,
      first_provider_event_ms INTEGER, first_delta_ms INTEGER,
      first_answer_delta_ms INTEGER, result_ms INTEGER
    )`);
    const timingColumns = new Set(ctx.storage.sql.exec<{ name: string }>("PRAGMA table_info(turn_timing)")
      .toArray().map(row => row.name));
    for (const [column, type] of [
      ["first_tool_call_ms", "INTEGER"], ["first_tool_result_ms", "INTEGER"],
      ["tool_calls", "INTEGER"], ["tool_duration_ms", "REAL"],
      ["post_tool_model_send_ms", "INTEGER"],
      ["first_model_call_ms", "INTEGER"], ["post_tool_model_call_ms", "INTEGER"],
    ]) {
      if (!timingColumns.has(column)) ctx.storage.sql.exec(`ALTER TABLE turn_timing ADD COLUMN ${column} ${type}`);
    }
    this.#toolTiming = new ToolTiming(ctx.storage.sql);
    this.#constructorMs = performance.now() - constructorStart;
  }

  async fetch(request: Request): Promise<Response> {
    const fetchStart = performance.now();
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
      const routeMs = performance.now() - fetchStart;
      return withSessionTiming(await this.#admitTurn(owner, turnId, input), `do_route;dur=${routeMs.toFixed(1)}, do_total;dur=${(performance.now() - fetchStart).toFixed(1)}`);
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
      const routeMs = performance.now() - fetchStart;
      return withSessionTiming(await this.#admitTurn(owner, key, body.input), `do_route;dur=${routeMs.toFixed(1)}, do_total;dur=${(performance.now() - fetchStart).toFixed(1)}`);
    }
    const turnId = /^\/turns\/([0-9a-f-]{36})$/.exec(url.pathname)?.[1];
    if (turnId && request.method === "GET") {
      const turn = this.#turn(turnId);
      const timing = turn && this.#timing(turnId);
      return turn ? reply(200, { turn_id: turnId, state: turn.state,
        tool_timing: this.#toolTiming.list(turnId),
        ...(timing ? { timing: { trace_id: timing.trace_id,
          agent_init_ms: timing.agent_init_ms, accepted_ms: timing.accepted_ms,
          model_send_ms: timing.model_send_ms,
          first_provider_event_ms: timing.first_provider_event_ms,
          first_delta_ms: timing.first_delta_ms,
          first_answer_delta_ms: timing.first_answer_delta_ms, result_ms: timing.result_ms,
          first_tool_call_ms: timing.first_tool_call_ms, first_tool_result_ms: timing.first_tool_result_ms,
          tool_calls: timing.tool_calls ?? 0, tool_duration_ms: timing.tool_duration_ms ?? 0,
          post_tool_model_send_ms: timing.post_tool_model_send_ms,
          first_model_call_ms: timing.first_model_call_ms, post_tool_model_call_ms: timing.post_tool_model_call_ms } } : {}),
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
    // The persisted wall-clock timeline survives DO eviction and makes
    // missing first-delta observations explicit. A failed initialization
    // resets it on the next attempt before a turn exists.
    const trace = crypto.randomUUID();
    this.ctx.storage.sql.exec(
      `INSERT INTO turn_timing (id, trace_id, started_at) VALUES (?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET trace_id = excluded.trace_id,
       started_at = excluded.started_at, agent_init_ms = NULL,
       accepted_ms = NULL, model_send_ms = NULL,
       first_provider_event_ms = NULL, first_delta_ms = NULL,
       first_answer_delta_ms = NULL, result_ms = NULL,
       first_tool_call_ms = NULL, first_tool_result_ms = NULL, tool_calls = NULL,
       tool_duration_ms = NULL, post_tool_model_send_ms = NULL,
       first_model_call_ms = NULL, post_tool_model_call_ms = NULL`,
      turnId, trace, Date.now(),
    );
    const traceId = this.#timing(turnId)!.trace_id;
    // Agent initialization must finish before the turn is persisted.
    const agentInitStart = performance.now();
    try { await this.#ready(owner, traceId); }
    catch {
      const unavailable = reply(503, { error: "model_unavailable" });
      unavailable.headers.set("server-timing", `agent_init;dur=${(performance.now() - agentInitStart).toFixed(1)}`);
      unavailable.headers.set("x-managed2-trace-id", traceId);
      return unavailable;
    }
    const initMs = performance.now() - agentInitStart;
    this.ctx.storage.sql.exec("UPDATE turn_timing SET agent_init_ms = ? WHERE id = ?", initMs, turnId);
    this.ctx.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES (?, ?, 'pending')", turnId, input);
    const admissionStart = performance.now();
    try {
      await this.#dispatch(turnId, input, owner);
      const accepted = reply(202, { turn_id: turnId, state: "accepted" });
      accepted.headers.set("server-timing", `agent_init;dur=${initMs.toFixed(1)}, admission;dur=${(performance.now() - admissionStart).toFixed(1)}`);
      accepted.headers.set("x-managed2-trace-id", traceId);
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

  #timing(id: string) {
    return this.ctx.storage.sql.exec<{ trace_id: string; started_at: number;
      agent_init_ms: number | null; accepted_ms: number | null;
      model_send_ms: number | null; first_provider_event_ms: number | null;
      first_delta_ms: number | null; first_answer_delta_ms: number | null; result_ms: number | null;
      first_tool_call_ms: number | null; first_tool_result_ms: number | null;
      tool_calls: number | null; tool_duration_ms: number | null; post_tool_model_send_ms: number | null;
      first_model_call_ms: number | null; post_tool_model_call_ms: number | null }>(
      "SELECT * FROM turn_timing WHERE id = ?", id,
    ).toArray()[0];
  }

  #modelSent(): void {
    // The existing transport observer runs after response.create was sent.
    // When turns overlap, attribution is ambiguous and stays null instead.
    if (this.#activeTraces.size !== 1) return;
    const external = this.#activeTraces.keys().next().value!;
    const timing = this.#timing(external);
    if (!timing) return;
    const elapsed = Math.max(0, Date.now() - timing.started_at);
    if (timing.model_send_ms === null) {
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET model_send_ms = ? WHERE id = ? AND model_send_ms IS NULL", elapsed, external,
      );
      console.info({ event: "managed2.model_send", trace_id: timing.trace_id, model_send_ms: elapsed });
    } else if (timing.first_tool_result_ms !== null && timing.post_tool_model_send_ms === null) {
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET post_tool_model_send_ms = ? WHERE id = ? AND post_tool_model_send_ms IS NULL", elapsed, external,
      );
      console.info({ event: "managed2.post_tool_model_send", trace_id: timing.trace_id, post_tool_model_send_ms: elapsed });
    }
  }

  #observeEvent(event: { type: string; payload: Record<string, unknown> }): void {
    if (event.type === "input.accepted") {
      const external = event.payload.request_id;
      const internal = event.payload.turn_id;
      if (typeof external === "string" && typeof internal === "string" && this.#activeTraces.has(external)) {
        this.#eventTurns.set(internal, external);
      }
      return;
    }
    const internal = event.payload.turn_id;
    const external = typeof internal === "string" ? this.#eventTurns.get(internal) : undefined;
    if (!external) return;
    const timing = this.#timing(external);
    if (!timing) return;
    const elapsed = Math.max(0, Date.now() - timing.started_at);
    if (event.type === "model.call.started") {
      const column = timing.first_tool_result_ms !== null ? "post_tool_model_call_ms" : "first_model_call_ms";
      this.ctx.storage.sql.exec(
        `UPDATE turn_timing SET ${column} = COALESCE(${column}, ?) WHERE id = ?`, elapsed, external,
      );
      console.info({ event: "managed2.model_call_started", trace_id: timing.trace_id,
        phase: timing.first_tool_result_ms !== null ? "after_tool" : "initial", elapsed_ms: elapsed });
      return;
    }
    if (event.type === "tool.call") {
      this.#toolTiming.observe(internal as string, external, "tool.call", event.payload, timing.started_at);
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET first_tool_call_ms = COALESCE(first_tool_call_ms, ?), tool_calls = COALESCE(tool_calls, 0) + 1 WHERE id = ?",
        elapsed, external,
      );
      console.info({ event: "managed2.tool_call", trace_id: timing.trace_id,
        call_id: event.payload.call_id, tool: event.payload.tool, elapsed_ms: elapsed });
      return;
    }
    if (event.type === "tool.result") {
      this.#toolTiming.observe(internal as string, external, "tool.result", event.payload, timing.started_at);
      const durationMs = typeof event.payload.duration_ns === "number" ? event.payload.duration_ns / 1e6 : 0;
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET first_tool_result_ms = COALESCE(first_tool_result_ms, ?), tool_duration_ms = COALESCE(tool_duration_ms, 0) + ? WHERE id = ?",
        elapsed, durationMs, external,
      );
      console.info({ event: "managed2.tool_result", trace_id: timing.trace_id,
        call_id: event.payload.call_id, tool: event.payload.tool, status: event.payload.status,
        elapsed_ms: elapsed, duration_ms: +durationMs.toFixed(1) });
      return;
    }
    if (event.type === "api.event" && event.payload.direction === "inbound") {
      if (timing.first_provider_event_ms === null) {
        this.ctx.storage.sql.exec(
          "UPDATE turn_timing SET first_provider_event_ms = ? WHERE id = ? AND first_provider_event_ms IS NULL",
          elapsed, external,
        );
        console.info({ event: "managed2.first_provider_event", trace_id: timing.trace_id,
          first_provider_event_ms: elapsed });
      }
      return;
    }
    if (event.type !== "assistant.delta"
      || typeof event.payload.text !== "string" || !event.payload.text) return;
    if (timing.first_delta_ms === null) {
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET first_delta_ms = ? WHERE id = ? AND first_delta_ms IS NULL", elapsed, external,
      );
      console.info({ event: "managed2.turn_first_delta", trace_id: timing.trace_id,
        first_delta_ms: elapsed });
    }
    if (event.payload.phase === "final_answer" && timing.first_answer_delta_ms === null) {
      this.ctx.storage.sql.exec(
        "UPDATE turn_timing SET first_answer_delta_ms = ? WHERE id = ? AND first_answer_delta_ms IS NULL", elapsed, external,
      );
      console.info({ event: "managed2.turn_first_answer_delta", trace_id: timing.trace_id,
        first_answer_delta_ms: elapsed });
      if (typeof internal === "string") this.#eventTurns.delete(internal);
    }
  }

  #turn(id: string) {
    return this.ctx.storage.sql.exec<{ id: string; input: string; state: string; message: string | null; error: string | null }>(
      "SELECT id, input, state, message, error FROM turns WHERE id = ?", id,
    ).toArray()[0];
  }

  #ready(owner: string, traceId?: string): Promise<Agent.Agent> {
    if (this.#agent) return this.#agent;
    const initTrace = traceId ?? crypto.randomUUID();
    const initStart = performance.now();
    let initializing = true;
const web = managedWeb({ egress: this.env.EGRESS, owner,
      onTiming: (context, phase, durationMs) => this.#toolTiming.phase(context, phase, durationMs),
    });
    const options = { tools: [currentTime, web].map(tool => this.#toolTiming.instrument(tool)), instructions: "You are a concise assistant." };
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalConfiguration"), { value: {
      model: "gpt-6-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false,
    } });
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), { value: {
      ...(this.env.RESPONSES_TRANSPORT === "websocket"
        ? { waitForPreconnect: true }
        : { inferenceForSession: () => ({ model: "gpt-6-sol", thinking: "low" }) }),
      subagentsEnabled: false,
      onResponseCreateSent: () => this.#modelSent(),
    } });
    return this.#agent = Agent.create({ ctx: this.ctx, env: { NANOCODEX: {
      fetch: (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
        const source = new Request(input, init);
        if (source.url !== "https://nanocodex.internal/v1/responses") {
          throw new Error("model transport requested an unexpected endpoint");
        }
        const headers = new Headers(source.headers);
        headers.set(OWNER_HEADER, owner);
        // The initial fetch is the persistent socket preconnection, not a
        // per-turn model request. A reconnect is correlated only if one turn
        // owns this DO at the moment of the fetch.
        const active = this.#activeTraces.size === 1 ? this.#activeTraces.values().next().value : undefined;
        const trace = active ?? (this.#activeTraces.size === 0
          ? initializing ? initTrace : crypto.randomUUID() : undefined);
        if (trace) headers.set("x-managed2-trace-id", trace);
        const began = performance.now();
        return this.env.EGRESS.fetch("https://api.openai.com/v1/responses", {
          method: source.method, headers, body: source.body, signal: source.signal,
          redirect: "manual",
        }).then(response => {
          const egressTiming = response.headers.get("server-timing") ?? "";
          const marker = /(?:^|, )egress_route;desc="(openai_api|chatgpt_subscription)"(?:,|$)/.exec(egressTiming)?.[1];
          const dispatch = /(?:^|, )egress_dispatch;dur=([0-9.]+)(?:,|$)/.exec(egressTiming)?.[1];
          console.info({ event: "managed2.model_route",
            ...(trace ? { trace_id: trace } : {}),
            phase: this.env.RESPONSES_TRANSPORT !== "websocket" ? "turn_http"
              : active ? "turn_reconnect" : initializing ? "preconnect" : "idle_reconnect",
            ...(active ? {} : { init_to_egress_ms: +(began - initStart).toFixed(1) }),
            egress_headers_ms: +(performance.now() - began).toFixed(1),
            status: response.status,
            route: marker === "openai_api" || marker === "chatgpt_subscription" ? marker : "unknown",
            ...(dispatch === undefined ? {} : { egress_dispatch_ms: Number(dispatch) }),
          });
          return response;
        });
      },
    } } }, options)
      .then(agent => {
        initializing = false;
        const watcher = agent.events.watch();
        watcher.onEvent(event => this.#observeEvent(event));
        console.info({ event: "managed2.agent_ready", trace_id: initTrace,
          agent_init_ms: +(performance.now() - initStart).toFixed(1),
          constructor_sql_ms: +this.#constructorMs.toFixed(1) });
        return agent;
      })
      .catch(error => { this.#agent = undefined; throw error; });
  }

  async #dispatch(id: string, input: string, owner: string): Promise<void> {
    if (this.#running.has(id)) return;
    const agent = await this.#ready(owner, this.#timing(id)?.trace_id);
    this.ctx.storage.sql.exec(
      "INSERT OR IGNORE INTO turn_timing (id, trace_id, started_at) VALUES (?, ?, ?)",
      id, crypto.randomUUID(), Date.now(),
    );
    const timing = this.#timing(id)!;
    this.#activeTraces.set(id, timing.trace_id);
    const turn = agent.turn.prompt({ id, input });
    try {
      await turn.accepted();
      this.ctx.storage.sql.exec("UPDATE turn_timing SET accepted_ms = COALESCE(accepted_ms, ?) WHERE id = ?",
        Math.max(0, Date.now() - timing.started_at), id);
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
          const resultMs = Math.max(0, Date.now() - timing.started_at);
          this.ctx.storage.sql.exec("UPDATE turn_timing SET result_ms = ? WHERE id = ?", resultMs, id);
          const observed = this.#timing(id);
          console.info({ event: "managed2.turn_result", trace_id: timing.trace_id,
            outcome: result ? "completed" : "failed", result_ms: resultMs,
            model_send_ms: observed?.model_send_ms ?? null,
            first_provider_event_ms: observed?.first_provider_event_ms ?? null,
            first_delta_ms: observed?.first_delta_ms ?? null,
            first_answer_delta_ms: observed?.first_answer_delta_ms ?? null,
            accepted_ms: observed?.accepted_ms ?? null,
            first_tool_call_ms: observed?.first_tool_call_ms ?? null,
            first_tool_result_ms: observed?.first_tool_result_ms ?? null,
            post_tool_model_send_ms: observed?.post_tool_model_send_ms ?? null,
            tool_calls: observed?.tool_calls ?? 0, tool_duration_ms: observed?.tool_duration_ms ?? 0,
            first_model_call_ms: observed?.first_model_call_ms ?? null,
            post_tool_model_call_ms: observed?.post_tool_model_call_ms ?? null });
          result?.dispose();
          turn.dispose();
          this.#running.delete(id);
          this.#activeTraces.delete(id);
          for (const [internal, external] of this.#eventTurns) {
            if (external === id) this.#eventTurns.delete(internal);
          }
        }
      })());
    } catch (error) {
      turn.dispose();
      this.#activeTraces.delete(id);
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

async function timedSessionFetch(stub: DurableObjectStub, input: RequestInfo, init: RequestInit, authTiming: string, requestStart: number): Promise<Response> {
  const start = performance.now();
  const response = await stub.fetch(input, init);
  return withSessionTiming(response, `${authTiming}, session;dur=${(performance.now() - start).toFixed(1)}, api_total;dur=${(performance.now() - requestStart).toFixed(1)}`);
}
function withSessionTiming(response: Response, timing: string): Response {
  if (response.status === 101) return response;
  const forwarded = new Response(response.body, response);
  forwarded.headers.set("server-timing", [response.headers.get("server-timing"), timing].filter(Boolean).join(", "));
  return forwarded;
}
