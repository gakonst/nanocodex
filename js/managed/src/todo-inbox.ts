import { initializeTodoDispositions, todoDispositionSnapshot, writeTodoDisposition, type TodoDispositionContext } from "./todo-dispositions";
import { enqueueTodoPreparation, initializeTodoPreparation, preparationView, scheduleTodoPreparation } from "./todo-preparation";
import { initializeGmailDecisionTraces, readGmailDecisionTraces, recentGmailTodoTraces } from "./gmail-firehose-traces";
import type { AccountAuthEnv, Principal } from "./account-auth";

const noStore = { "cache-control": "no-store" };
const reply = (data: unknown, status = 200) => Response.json(data, { status, headers: noStore });
// Persisted decisions/traces demonstrate observed local work only. No account
// watch/import heartbeat is verified here; empty queues NEVER prove caught-up.
const unknownSourceCoverage = () => ({
  status: "unknown", complete: false, background_ingest: "unknown",
  reason: "watch_import_health_unverified",
  scope: "Bounded persisted account records only; not the whole mailbox.",
});
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

type ItemRow = { id: string; body: string; watch_hint: string; status: string; version: number; created_at: string; operation_id: string };
type DecisionRow = { id: string; todo_id: string | null; workflow_id: string | null; title: string; context: string; source_label: string; source_url: string; source_connection_id: string | null; source_thread_id: string | null; source_message_id: string | null; status: string; version: number; choices: string; created_at: string };
type DecisionView = Omit<DecisionRow, "workflow_id">;
type Choice = { id: string; title: string };

async function boundedBody(request: Request): Promise<Record<string, unknown> | undefined> {
  if (!request.body) return undefined;
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > 8192) return undefined;
      chunks.push(value);
    }
  } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  const data = new Uint8Array(bytes);
  let offset = 0;
  for (const chunk of chunks) { data.set(chunk, offset); offset += chunk.length; }
  try {
    const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(data));
    return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed as Record<string, unknown> : undefined;
  } catch { return undefined; }
}

export function initializeTodoInbox(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS todo_captures (
    id TEXT PRIMARY KEY, operation_id TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL, watch_hint TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'captured', version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS todo_decisions (
    id TEXT PRIMARY KEY, source_key TEXT NOT NULL UNIQUE, todo_id TEXT, workflow_id TEXT,
    title TEXT NOT NULL, context TEXT NOT NULL, source_label TEXT NOT NULL, source_url TEXT NOT NULL,
    choices TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'needs_you',
    version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS todo_capture_updates (
    operation_id TEXT PRIMARY KEY, item_id TEXT NOT NULL, version INTEGER NOT NULL,
    status TEXT NOT NULL, item_json TEXT NOT NULL, recorded_at TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS todo_decision_responses (
    operation_id TEXT PRIMARY KEY, decision_id TEXT NOT NULL, version INTEGER NOT NULL,
    choice_id TEXT, text TEXT, recorded_at TEXT NOT NULL
  );`);
  // Additive and safe for existing accounts and repeated initialization.
  const columns = new Set(storage.sql.exec<{ name: string }>("PRAGMA table_info(todo_decisions)").toArray().map(column => column.name));
  for (const column of ["source_connection_id", "source_thread_id", "source_message_id"]) {
    if (!columns.has(column)) storage.sql.exec(`ALTER TABLE todo_decisions ADD COLUMN ${column} TEXT`);
  }
  initializeTodoDispositions(storage);
  initializeGmailDecisionTraces(storage);
  initializeTodoPreparation(storage);
}

export async function handleTodoInbox(request: Request, storage: DurableObjectStorage, dispositionContext?: TodoDispositionContext): Promise<Response> {
  const url = new URL(request.url), path = url.pathname;
  if (request.method === "HEAD") return reply({ error: "invalid_request" }, 400);
  if (path === "/todo/traces" && request.method === "GET") return readGmailDecisionTraces(storage, url.searchParams);
  if (url.search) return reply({ error: "invalid_request" }, 400);
  if (path === "/todo" && request.method === "GET") {
    const items = storage.sql.exec<ItemRow>("SELECT * FROM todo_captures ORDER BY created_at DESC LIMIT 200").toArray()
      .map(item => ({ ...item, preparation: preparationView(storage, "capture", item.id) }));
    // Completed activity must never crowd an older unanswered choice out.
    const projection = "id, todo_id, title, context, source_label, source_url, source_connection_id, source_thread_id, source_message_id, choices, status, version, created_at";
    const open = storage.sql.exec<DecisionView>(`SELECT ${projection} FROM todo_decisions WHERE status IN ('needs_you','preparing') ORDER BY created_at DESC LIMIT 200`).toArray();
    const activity = storage.sql.exec<DecisionView>(`SELECT ${projection} FROM todo_decisions WHERE status NOT IN ('needs_you','preparing') ORDER BY created_at DESC LIMIT 200`).toArray();
    const decisions = [...open, ...activity].map(({ choices, ...rest }) => ({ ...rest, choices: JSON.parse(choices) as Choice[], preparation: preparationView(storage, "decision", rest.id) }));
    return reply({ ...todoDispositionSnapshot(storage), source_coverage: unknownSourceCoverage(), items, decisions, traces: recentGmailTodoTraces(storage),
      feed_bounds: { traces: "recent", trace_limit: 100 } });
  }
  if (path === "/todo/snooze" && request.method === "POST") {
    const input = await boundedBody(request);
    return input ? writeTodoDisposition(input, storage, dispositionContext) : reply({ error: "invalid_json" }, 400);
  }
  if (path === "/todo" && request.method === "POST") {
    const input = await boundedBody(request);
    if (!input) return reply({ error: "invalid_json" }, 400);
    const body = input.body, watchHint = input.watch_hint ?? "", operationInput = input.operation_id;
    if (typeof body !== "string" || !body.trim() || new TextEncoder().encode(body).length > 4096
      || typeof watchHint !== "string" || new TextEncoder().encode(watchHint).length > 1024
      || typeof operationInput !== "string" || !uuid.test(operationInput)) return reply({ error: "invalid_capture" }, 400);
    const operation = operationInput.toLowerCase();
    const previous = storage.sql.exec<ItemRow>("SELECT * FROM todo_captures WHERE operation_id = ?", operation).toArray()[0];
    if (previous) {
      if (previous.body !== body || previous.watch_hint !== watchHint) return reply({ error: "operation_conflict" }, 409);
      await scheduleTodoPreparation(storage);
      return reply({ item: { ...previous, preparation: preparationView(storage, "capture", previous.id) } });
    }
    const id = crypto.randomUUID(), at = new Date().toISOString();
    storage.transactionSync(() => {
      storage.sql.exec("INSERT INTO todo_captures(id, operation_id, body, watch_hint, created_at) VALUES (?, ?, ?, ?, ?)", id, operation, body, watchHint, at);
      enqueueTodoPreparation(storage, "capture", id, 1);
    });
    await scheduleTodoPreparation(storage);
    return reply({ item: { ...storage.sql.exec<ItemRow>("SELECT * FROM todo_captures WHERE id = ?", id).toArray()[0], preparation: preparationView(storage, "capture", id) } }, 201);
  }
  // A detail read is a persisted snapshot only; it must not kick off inference.
  const detail = path.match(/^\/todo\/(items|decisions)\/([0-9a-f-]{36})$/i);
  if (detail && request.method === "GET" && uuid.test(detail[2]!)) {
    const kind = detail[1] === "items" ? "capture" : "decision", id = detail[2]!.toLowerCase();
    const row = storage.sql.exec<Record<string, any>>(`SELECT * FROM ${kind === "capture" ? "todo_captures" : "todo_decisions"} WHERE id=?`, id).toArray()[0];
    if (!row) return reply({ error: "not_found" }, 404);
    delete row.workflow_id; delete row.source_key;
    if (kind === "decision") row.choices = JSON.parse(row.choices);
    return reply({ source_coverage: unknownSourceCoverage(), [kind === "capture" ? "item" : "decision"]: { ...row, preparation: preparationView(storage, kind, id) } });
  }
  const prepare = path.match(/^\/todo\/(items|decisions)\/([0-9a-f-]{36})\/prepare$/i);
  if (prepare && request.method === "POST" && uuid.test(prepare[2]!)) {
    const input = await boundedBody(request);
    if (!input) return reply({ error: "invalid_json" }, 400);
    const { version, operation_id: op, text } = input;
    if (Object.keys(input).some(key => !["version", "operation_id", "text"].includes(key))
      || !Number.isSafeInteger(version) || (version as number) < 1
      || typeof op !== "string" || !uuid.test(op) || typeof text !== "string" || !text.trim()
      || new TextEncoder().encode(text).length > 4096) return reply({ error: "invalid_preparation_request" }, 400);
    const kind = prepare[1] === "items" ? "capture" : "decision", id = prepare[2]!.toLowerCase(), operation = op.toLowerCase();
    const result = storage.transactionSync(() => {
      const previous = storage.sql.exec<{ kind: string; target_id: string; version: number; instructions: string; response: string }>("SELECT * FROM todo_preparation_requests WHERE operation_id=?", operation).toArray()[0];
      if (previous) return previous.kind === kind && previous.target_id === id && previous.version === version && previous.instructions === text
        ? reply(JSON.parse(previous.response)) : reply({ error: "operation_conflict" }, 409);
      const row = storage.sql.exec<{ version: number; status: string }>(`SELECT version,status FROM ${kind === "capture" ? "todo_captures" : "todo_decisions"} WHERE id=?`, id).toArray()[0];
      if (!row) return reply({ error: "not_found" }, 404);
      if (row.version !== version || !(kind === "capture" ? ["captured"] : ["needs_you", "preparing"]).includes(row.status)) return reply({ error: "stale_preparation" }, 409);
      if (kind === "decision" && storage.sql.exec(`SELECT s.operation_id FROM todo_mail_sends s
        JOIN todo_mail_decision_drafts d ON d.draft_id=s.draft_id WHERE d.decision_id=? LIMIT 1`, id).toArray().length) return reply({ error: "decision_send_locked" }, 409);
      if (kind === "decision") storage.sql.exec("UPDATE todo_decisions SET status='preparing',version=version+1 WHERE id=?", id);
      const nextVersion = (version as number) + (kind === "decision" ? 1 : 0);
      const response = { version: nextVersion, preparation: enqueueTodoPreparation(storage, kind, id, nextVersion, text) };
      storage.sql.exec("INSERT INTO todo_preparation_requests(operation_id,kind,target_id,version,instructions,response) VALUES(?,?,?,?,?,?)", operation, kind, id, version as number, text, JSON.stringify(response));
      return reply(response, 202);
    });
    await scheduleTodoPreparation(storage);
    return result;
  }
  const itemMatch = path.match(/^\/todo\/items\/([0-9a-f-]{36})$/i);
  if (itemMatch && request.method === "PATCH" && uuid.test(itemMatch[1]!)) {
    const input = await boundedBody(request);
    if (!input) return reply({ error: "invalid_json" }, 400);
    const { operation_id: operationInput, version, status } = input;
    if (typeof operationInput !== "string" || !uuid.test(operationInput)
      || !Number.isSafeInteger(version) || (version as number) < 1
      || (status !== "done" && status !== "captured" && status !== "parked")) return reply({ error: "invalid_update" }, 400);
    const id = itemMatch[1]!.toLowerCase(), operation = operationInput.toLowerCase();
    // Keep validation and the durable receipt in the same transaction as the mutation.
    const result = storage.transactionSync(() => {
      const previous = storage.sql.exec<{ item_id: string; version: number; status: string; item_json: string }>(
        "SELECT * FROM todo_capture_updates WHERE operation_id = ?", operation).toArray()[0];
      if (previous) return previous.item_id === id && previous.version === version && previous.status === status
        ? reply({ item: JSON.parse(previous.item_json) }) : reply({ error: "operation_conflict" }, 409);
      const item = storage.sql.exec<ItemRow>("SELECT * FROM todo_captures WHERE id = ?", id).toArray()[0];
      if (!item) return reply({ error: "not_found" }, 404);
      if (item.version !== version) return reply({ error: "stale_item" }, 409);
      if (!["captured", "done", "parked"].includes(item.status) || item.status === status)
        return reply({ error: "invalid_transition" }, 409);
      storage.sql.exec("UPDATE todo_captures SET status = ?, version = version + 1 WHERE id = ? AND version = ?", status, id, version);
      if (status === "captured") enqueueTodoPreparation(storage, "capture", id, item.version + 1);
      else storage.sql.exec("UPDATE todo_preparations SET state='blocked',generation=generation+1,result=?,updated_at=? WHERE kind='capture' AND target_id=?", JSON.stringify({ error: status === "parked" ? "preparation_parked" : "capture_completed" }), new Date().toISOString(), id);
      const updated = { ...item, status, version: item.version + 1, preparation: preparationView(storage, "capture", id) };
      storage.sql.exec("INSERT INTO todo_capture_updates(operation_id, item_id, version, status, item_json, recorded_at) VALUES (?, ?, ?, ?, ?, ?)",
        operation, id, version, status, JSON.stringify(updated), new Date().toISOString());
      return reply({ item: updated });
    });
    await scheduleTodoPreparation(storage);
    return result;
  }
  const response = path.match(/^\/todo\/decisions\/([0-9a-f-]{36})\/respond$/i);
  if (response && request.method === "POST" && uuid.test(response[1]!)) {
    const input = await boundedBody(request);
    if (!input) return reply({ error: "invalid_json" }, 400);
    const operationInput = input.operation_id, version = input.version,
      choice = input.choice_id, text = input.text;
    if (typeof operationInput !== "string" || !uuid.test(operationInput)
      || !Number.isSafeInteger(version) || (version as number) < 1
      || (typeof choice !== "string" && choice !== null)
      || (typeof text !== "string" && text !== null)
      || (typeof choice === "string") === (typeof text === "string")
      || typeof text === "string" && (!text.trim() || new TextEncoder().encode(text).length > 4096))
      return reply({ error: "invalid_response" }, 400);
    const id = response[1]!.toLowerCase(), operation = (operationInput as string).toLowerCase();
    const previous = storage.sql.exec<{ decision_id: string; version: number; choice_id: string | null; text: string | null }>(
      "SELECT * FROM todo_decision_responses WHERE operation_id = ?", operation).toArray()[0];
    if (previous) return previous.decision_id === id && previous.version === version
      && previous.choice_id === choice && previous.text === text
      ? reply({ status: "recorded" }) : reply({ error: "operation_conflict" }, 409);
    const decision = storage.sql.exec<DecisionRow>("SELECT * FROM todo_decisions WHERE id = ?", id).toArray()[0];
    if (!decision) return reply({ error: "not_found" }, 404);
    if (decision.version !== version || decision.status !== "needs_you") return reply({ error: "stale_decision" }, 409);
    if (typeof choice === "string" && !(JSON.parse(decision.choices) as Choice[]).some(option => option.id === choice))
      return reply({ error: "invalid_choice" }, 400);
    storage.transactionSync(() => {
      storage.sql.exec("INSERT INTO todo_decision_responses(operation_id, decision_id, version, choice_id, text, recorded_at) VALUES (?, ?, ?, ?, ?, ?)",
        operation, id, version, choice ?? null, text ?? null, new Date().toISOString());
      storage.sql.exec("UPDATE todo_decisions SET status = 'answered', version = version + 1 WHERE id = ? AND version = ? AND status = 'needs_you'", id, version);
      storage.sql.exec("UPDATE todo_preparations SET state='blocked',generation=generation+1,result=?,updated_at=? WHERE kind='decision' AND target_id=?", JSON.stringify({error:"decision_answered"}), new Date().toISOString(), id);
    });
    // An answered decision remains pending workflow consumption, not completed.
    // A recorded answer is NOT an executed workflow step; the producer owns
    // authorization, revalidation, and any external side effects.
    return reply({ status: "recorded" });
  }
  return reply({ error: "not_found" }, 404);
}

/** Exposed only at the authenticated managed account route, never at a public DO path. */
export async function routeTodoRequest(request: Request, env: Pick<AccountAuthEnv, "NANOCODEX_USERS">,
  url: URL, principal: Principal | null | undefined): Promise<Response | null> {
  if (!url.pathname.startsWith("/v1/todo")) return null;
  if (!principal) return reply({ error: "unauthorized" }, 401);
  if (principal.connectGrant || !["api_key", "account_session"].includes(principal.kind)
    || !principal.capabilities.includes(request.method === "GET" ? "agents:read" : "agents:write"))
    return reply({ error: "forbidden" }, 403);
  if (request.method !== "GET") {
    if (principal.kind === "account_session" && request.headers.get("origin") !== url.origin)
      return reply({ error: "forbidden_origin" }, 403);
  }
  const sourceHealth = url.pathname === "/v1/todo/source-health";
  if (sourceHealth && request.method !== "GET") return reply({ error: "method_not_allowed" }, 405);
  const mailOrSchedule = /^\/v1\/todo\/(?:schedule|mail\/(?:accounts|threads(?:\/[A-Za-z0-9_-]+(?:\/modify)?)?|drafts(?:\/[0-9a-fA-F-]{36})?|send|suggest|messages\/[A-Za-z0-9_-]+\/attachments\/[A-Za-z0-9_-]+))$/.test(url.pathname);
  if (!sourceHealth && !mailOrSchedule && (!/^\/v1\/todo(?:$|\/snooze$|\/traces$|\/items\/[0-9a-f-]{36}(?:\/prepare)?$|\/decisions\/[0-9a-f-]{36}(?:\/(?:respond|prepare))?$)/i.test(url.pathname)
    || url.search && url.pathname !== "/v1/todo/traces")) return reply({ error: "not_found" }, 404);
  const path = url.pathname.slice(3);
  return env.NANOCODEX_USERS.getByName(principal.userId).fetch(
    `https://user.internal${path}${url.search}`, new Request(request, { headers: request.headers.has("content-type") ? { "content-type": request.headers.get("content-type")! } : {} }),
  );
}

export type TodoDecisionProposal = Readonly<{
  source_key: string; todo_id?: string; workflow_id?: string;
  title: string; context: string; source_label: string;
  source_url: string; choices: readonly Choice[];
  prepare?: boolean;
  source_connection_id?: string | null; source_thread_id?: string | null; source_message_id?: string | null;
}>;

/** Internal DO RPC for future firehose/workflow producers; not a public client route. */
export function proposeTodoDecision(storage: DurableObjectStorage, input: TodoDecisionProposal):
  Pick<DecisionRow, "id" | "status" | "version"> {
  const safe = (value: unknown, limit: number): value is string => typeof value === "string"
    && value.trim().length > 0 && new TextEncoder().encode(value).length <= limit;
  if (!safe(input.source_key, 256)
    || input.todo_id !== undefined && (!uuid.test(input.todo_id)
      || !storage.sql.exec("SELECT id FROM todo_captures WHERE id = ?", input.todo_id).toArray().length)
    || input.workflow_id !== undefined && !safe(input.workflow_id, 256)
    || input.source_connection_id != null && !safe(input.source_connection_id, 256)
    || input.source_thread_id != null && (typeof input.source_thread_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(input.source_thread_id))
    || input.source_message_id != null && (typeof input.source_message_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(input.source_message_id))
    || input.prepare !== undefined && typeof input.prepare !== "boolean"
    || !safe(input.title, 200)
    || !safe(input.context, 4096) || !safe(input.source_label, 120)
    || typeof input.source_url !== "string" || input.source_url.length > 2048
    || input.source_url && !input.source_url.startsWith("https://")
    || !Array.isArray(input.choices) || input.choices.length < 1 || input.choices.length > 4
    || input.choices.some(choice => !safe(choice.id, 60) || !safe(choice.title, 120))
    || new Set(input.choices.map(choice => choice.id)).size !== input.choices.length) {
    throw new Error("invalid todo decision proposal");
  }
  const id = crypto.randomUUID();
  storage.transactionSync(() => {
  storage.sql.exec(`INSERT INTO todo_decisions
    (id, source_key, todo_id, workflow_id, title, context, source_label, source_url, choices, created_at, source_connection_id, source_thread_id, source_message_id, status)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(source_key) DO NOTHING`, id,
  input.source_key, input.todo_id ?? null, input.workflow_id ?? null,
  input.title, input.context, input.source_label, input.source_url,
  JSON.stringify(input.choices), new Date().toISOString(), input.source_connection_id ?? null, input.source_thread_id ?? null, input.source_message_id ?? null, input.prepare ? "preparing" : "needs_you");
  if (input.prepare && storage.sql.exec("SELECT id FROM todo_decisions WHERE id=?", id).toArray().length) enqueueTodoPreparation(storage, "decision", id, 1);
  });
  const existing = storage.sql.exec<DecisionRow>("SELECT * FROM todo_decisions WHERE source_key = ?", input.source_key).toArray()[0];
  if (!existing) throw new Error("todo proposal persistence failed");
  // A replay cannot silently redefine an already presented approval or its choices.
  if (existing.todo_id !== (input.todo_id ?? null) || existing.workflow_id !== (input.workflow_id ?? null)
    || existing.title !== input.title || existing.context !== input.context
    || existing.source_label !== input.source_label || existing.source_url !== input.source_url
    || existing.choices !== JSON.stringify(input.choices)) throw new Error("todo_source_conflict");
  const sourceFields = ["source_connection_id", "source_thread_id", "source_message_id"] as const;
  if (sourceFields.some(field => input[field] != null && existing[field] != null && input[field] !== existing[field]))
    throw new Error("todo_source_conflict");
  // Exact producer references can enrich old rows; older producers cannot erase them.
  storage.sql.exec(`UPDATE todo_decisions SET source_connection_id = COALESCE(source_connection_id, ?),
    source_thread_id = COALESCE(source_thread_id, ?), source_message_id = COALESCE(source_message_id, ?) WHERE id = ?`,
  input.source_connection_id ?? null, input.source_thread_id ?? null, input.source_message_id ?? null, existing.id);
  return { id: existing.id, status: existing.status, version: existing.version };
}
