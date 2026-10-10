import { screenResult, screenResultMatches, type AgentScreenResult, type ScreenResultShape, type ScreenTarget } from "./hand-remote-agent";
import type { HandRemoteLateResult } from "./hand-remote";

/**
 * Settled screen payloads (one JPEG or recording each) are retained briefly for
 * receipt reads: per session (one agent's CUA loop cannot expire another's
 * lost response) and within one byte budget for the whole owner object.
 */
const PAYLOAD_TTL_MS = 10 * 60_000;
const SESSION_PAYLOAD_LIMIT = 16;
const PAYLOAD_BYTE_BUDGET = 32 * 1024 * 1024;

type Row = {
  session_id: string; call_id: string; name: string; route_token: string; request_id: string;
  connection_id: string; generation: string; target_json: string; expects_image: number; expects_recording: number; input_digest: string; state: "running" | "settled" | "expired";
  result_json: string | null; created_at: number; deadline_at: number; settled_at: number | null;
};

/** inputDigest distinguishes a repeated identity (replay) from a conflicting reuse of its call ID. */
export type ScreenCallIdentity = Readonly<{ sessionId: string; callId: string; name: string; routeToken: string; inputDigest: string }>;
/** The admitted result shape is retained so a late result is validated exactly like a live one. */
export type ScreenCallAdmission = Readonly<{ requestId: string; connectionId: string; generation: string; target: ScreenTarget; deadlineAt: number }> & ScreenResultShape;
export type ScreenReceipt =
  | Readonly<{ state: "missing" }>
  | Readonly<{ state: "mismatch" }>
  | Readonly<{ state: "conflict" }>
  | Readonly<{ state: "running"; deadlineAt: number }>
  | Readonly<{ state: "unresolved" }>
  | Readonly<{ state: "settled"; result: ReturnType<typeof screenResult> }>;

/**
 * Durable ledger of admitted screen actions, keyed by exact source identity.
 * A row is written synchronously before the agent_call frame is sent and is
 * never deleted, so "no row" proves this identity was never sent here. Only
 * payloads expire; an expired identity reads as unresolved, never missing.
 * A result is recorded only when the host or the broker actually produced
 * one; a call whose broker instance was lost stays running until its deadline
 * and then reads as unresolved.
 */
export class ScreenCallLedger {
  readonly #waiters = new Map<string, { promise: Promise<void>; resolve(): void }>();
  constructor(private readonly sql: SqlStorage, private readonly now: () => number = Date.now) {
    sql.exec(`CREATE TABLE IF NOT EXISTS hosted_screen_calls (
      session_id TEXT NOT NULL, call_id TEXT NOT NULL, name TEXT NOT NULL, route_token TEXT NOT NULL,
      request_id TEXT NOT NULL UNIQUE, connection_id TEXT NOT NULL, generation TEXT NOT NULL, target_json TEXT NOT NULL,
      expects_image INTEGER NOT NULL, expects_recording INTEGER NOT NULL, input_digest TEXT NOT NULL,
      state TEXT NOT NULL, result_json TEXT, created_at INTEGER NOT NULL, deadline_at INTEGER NOT NULL, settled_at INTEGER,
      PRIMARY KEY(session_id, call_id)
    )`);
  }

  #row(sessionId: string, callId: string): Row | undefined {
    return this.sql.exec<Row>("SELECT * FROM hosted_screen_calls WHERE session_id=? AND call_id=?", sessionId, callId).toArray()[0];
  }

  has(sessionId: string, callId: string): boolean { return this.#row(sessionId, callId) !== undefined; }

  /** Synchronous: the fence check, duplicate check and insert cannot interleave with another request. */
  admit(identity: ScreenCallIdentity, call: ScreenCallAdmission, fenced: () => boolean): "admitted" | "fenced" | "duplicate" {
    if (fenced()) return "fenced";
    if (this.#row(identity.sessionId, identity.callId)) return "duplicate";
    const target = JSON.stringify({ machine_id: call.target.machine_id, machine_name: call.target.machine_name, id: call.target.id });
    this.sql.exec("INSERT INTO hosted_screen_calls(session_id,call_id,name,route_token,request_id,connection_id,generation,target_json,expects_image,expects_recording,input_digest,state,created_at,deadline_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,'running',?,?)",
      identity.sessionId, identity.callId, identity.name, identity.routeToken, call.requestId, call.connectionId, call.generation,
      target, call.expectsImage ? 1 : 0, call.recording ? 1 : 0, identity.inputDigest, this.now(), call.deadlineAt);
    let resolve!: () => void;
    const promise = new Promise<void>(done => { resolve = done; });
    this.#waiters.set(call.requestId, { promise, resolve });
    return "admitted";
  }

  /** Called inside the broker's finish(), before its response is built. */
  settle(requestId: string, result: AgentScreenResult): void {
    try {
      this.sql.exec("UPDATE hosted_screen_calls SET state='settled', result_json=?, settled_at=? WHERE request_id=? AND state='running'",
        JSON.stringify(rawResult(result)), this.now(), requestId);
      this.#prune(requestId);
    } finally {
      this.#waiters.get(requestId)?.resolve();
      this.#waiters.delete(requestId);
    }
  }

  /**
   * A host result after this instance lost its pending call: only for the same
   * connection and generation, and only when it satisfies the same result-shape
   * rules a live pending call enforces. A rejected result leaves the identity
   * running, so it reads as unresolved after its deadline.
   */
  late(late: HandRemoteLateResult): boolean {
    if (this.#waiters.has(late.requestId)) return false;
    const row = this.sql.exec<Row>("SELECT * FROM hosted_screen_calls WHERE request_id=?", late.requestId).toArray()[0];
    if (!row || row.state !== "running" || row.connection_id !== late.connectionId || row.generation !== late.generation) return false;
    if (!screenResultMatches(late.result, { expectsImage: row.expects_image === 1, recording: row.expects_recording === 1 })) return false;
    this.sql.exec("UPDATE hosted_screen_calls SET state='settled', result_json=?, settled_at=? WHERE request_id=? AND state='running'",
      JSON.stringify(rawResult(late.result)), this.now(), late.requestId);
    this.#prune(late.requestId);
    return true;
  }

  /** Cancellation state observed before the abort is delivered. */
  cancelState(sessionId: string, callId: string): "requested" | "not_delivered" | "terminal" | undefined {
    const row = this.#row(sessionId, callId);
    if (!row) return undefined;
    if (row.state !== "running") return "terminal";
    return this.#waiters.has(row.request_id) ? "requested" : "not_delivered";
  }

  /** In-memory evidence that the original call is still owned by this instance. */
  inFlight(sessionId: string, callId: string): boolean {
    const row = this.#row(sessionId, callId);
    return !!row && this.#waiters.has(row.request_id);
  }

  /** Receipt-only read of one identity's original route; waits bounded on the original pending call. */
  async receipt(sessionId: string, callId: string, routeToken: string, name: string | undefined, waitMs: number, inputDigest?: string): Promise<ScreenReceipt> {
    let row = this.#row(sessionId, callId);
    if (!row) return { state: "missing" };
    if (row.route_token !== routeToken || (name !== undefined && row.name !== name)) return { state: "mismatch" };
    if (inputDigest !== undefined && row.input_digest !== inputDigest) return { state: "conflict" };
    if (row.state === "running") {
      const waiter = this.#waiters.get(row.request_id);
      const remaining = Math.max(0, Math.min(waitMs, row.deadline_at + 1_000 - this.now()));
      if (waiter && remaining > 0) {
        let timer: ReturnType<typeof setTimeout> | undefined;
        await Promise.race([waiter.promise, new Promise<void>(done => { timer = setTimeout(done, remaining); })])
          .finally(() => clearTimeout(timer));
        row = this.#row(sessionId, callId)!;
      }
      if (row.state === "running") {
        // Without its pending call (instance lost) the identity may still settle
        // from a late host result until its own deadline; then it is unknown.
        if (!this.#waiters.has(row.request_id) && this.now() >= row.deadline_at) return { state: "unresolved" };
        return { state: "running", deadlineAt: row.deadline_at };
      }
    }
    if (row.state !== "settled" || row.result_json === null) return { state: "unresolved" };
    const target = JSON.parse(row.target_json) as ScreenTarget;
    return { state: "settled", result: screenResult(JSON.parse(row.result_json) as AgentScreenResult, target) };
  }

  /** Expires payloads only; identity rows stay, so expiry reads as unresolved, never missing. */
  #prune(settledRequestId: string): void {
    const expire = "UPDATE hosted_screen_calls SET state='expired', result_json=NULL WHERE state='settled' AND ";
    this.sql.exec(expire + "settled_at < ?", this.now() - PAYLOAD_TTL_MS);
    const session = this.sql.exec<{ session_id: string }>("SELECT session_id FROM hosted_screen_calls WHERE request_id=?", settledRequestId).toArray()[0]?.session_id;
    if (session !== undefined) {
      this.sql.exec(expire + `session_id=? AND request_id NOT IN (SELECT request_id FROM hosted_screen_calls
        WHERE state='settled' AND session_id=? ORDER BY settled_at DESC LIMIT ?)`, session, session, SESSION_PAYLOAD_LIMIT);
    }
    let bytes = 0;
    for (const row of this.sql.exec<{ request_id: string; bytes: number }>(
      "SELECT request_id, length(result_json) AS bytes FROM hosted_screen_calls WHERE state='settled' ORDER BY settled_at DESC").toArray()) {
      bytes += row.bytes;
      if (bytes > PAYLOAD_BYTE_BUDGET && row.request_id !== settledRequestId) this.sql.exec(expire + "request_id=?", row.request_id);
    }
  }
}

/** The host's exact validated result fields, stored once. */
function rawResult(result: AgentScreenResult): AgentScreenResult {
  return { status: result.status, ...(result.jpeg === undefined ? {} : { jpeg: result.jpeg, width: result.width, height: result.height }),
    ...(result.observation === undefined ? {} : { observation: result.observation }),
    ...(result.recording === undefined ? {} : { recording: result.recording }) };
}
