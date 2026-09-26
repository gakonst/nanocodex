import { durablePlacementOptions } from "nanocodex/cloudflare/durable-placement";
import type { AccountAuthEnv, Principal } from "./account-auth";

const noStore = { "cache-control": "no-store" };
const reply = (data: unknown, status = 200) => Response.json(data, { status, headers: noStore });
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

type ItemRow = { id: string; body: string; watch_hint: string; status: string; version: number; created_at: string; operation_id: string };
type DecisionRow = { id: string; todo_id: string | null; workflow_id: string | null; title: string; context: string; source_label: string; source_url: string; status: string; version: number; choices: string; created_at: string };
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
  CREATE TABLE IF NOT EXISTS todo_decision_responses (
    operation_id TEXT PRIMARY KEY, decision_id TEXT NOT NULL, version INTEGER NOT NULL,
    choice_id TEXT, text TEXT, recorded_at TEXT NOT NULL
  );`);
}

export async function handleTodoInbox(request: Request, storage: DurableObjectStorage): Promise<Response> {
  const url = new URL(request.url), path = url.pathname;
  if (url.search || request.method === "HEAD") return reply({ error: "invalid_request" }, 400);
  if (path === "/todo" && request.method === "GET") {
    const items = storage.sql.exec<ItemRow>("SELECT * FROM todo_captures ORDER BY created_at DESC LIMIT 200").toArray();
    // Completed activity must never crowd an older unanswered choice out.
    const projection = "id, todo_id, title, context, source_label, source_url, choices, status, version, created_at";
    const open = storage.sql.exec<DecisionView>(`SELECT ${projection} FROM todo_decisions WHERE status = 'needs_you' ORDER BY created_at DESC LIMIT 200`).toArray();
    const activity = storage.sql.exec<DecisionView>(`SELECT ${projection} FROM todo_decisions WHERE status != 'needs_you' ORDER BY created_at DESC LIMIT 200`).toArray();
    const decisions = [...open, ...activity].map(({ choices, ...rest }) => ({ ...rest, choices: JSON.parse(choices) as Choice[] }));
    return reply({ items, decisions });
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
    if (previous) return previous.body === body && previous.watch_hint === watchHint
      ? reply({ item: previous }) : reply({ error: "operation_conflict" }, 409);
    const id = crypto.randomUUID(), at = new Date().toISOString();
    storage.sql.exec("INSERT INTO todo_captures(id, operation_id, body, watch_hint, created_at) VALUES (?, ?, ?, ?, ?)", id, operation, body, watchHint, at);
    return reply({ item: storage.sql.exec<ItemRow>("SELECT * FROM todo_captures WHERE id = ?", id).toArray()[0] }, 201);
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
    });
    // An answered decision remains pending workflow consumption, not completed.
    // A recorded answer is NOT an executed workflow step; the producer owns
    // authorization, revalidation, and any external side effects.
    return reply({ status: "recorded" });
  }
  return reply({ error: "not_found" }, 404);
}

/** Exposed only at the authenticated managed account route, never at a public DO path. */
export async function routeTodoRequest(request: Request, env: Pick<AccountAuthEnv, "NANOCODEX_USERS" | "trustedClientIngressColo">,
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
  if (url.search || !/^\/v1\/todo(?:$|\/decisions\/[0-9a-f-]{36}\/respond$)/i.test(url.pathname))
    return reply({ error: "not_found" }, 404);
  const path = url.pathname.slice(3);
  return env.NANOCODEX_USERS.getByName(principal.userId, durablePlacementOptions(env.trustedClientIngressColo)).fetch(
    `https://user.internal${path}`, new Request(request, { headers: { "content-type": "application/json" } }),
  );
}

export type TodoDecisionProposal = Readonly<{
  source_key: string; todo_id?: string; workflow_id?: string;
  title: string; context: string; source_label: string;
  source_url: string; choices: readonly Choice[];
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
  storage.sql.exec(`INSERT INTO todo_decisions
    (id, source_key, todo_id, workflow_id, title, context, source_label, source_url, choices, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(source_key) DO NOTHING`, id,
  input.source_key, input.todo_id ?? null, input.workflow_id ?? null,
  input.title, input.context, input.source_label, input.source_url,
  JSON.stringify(input.choices), new Date().toISOString());
  const existing = storage.sql.exec<DecisionRow>("SELECT * FROM todo_decisions WHERE source_key = ?", input.source_key).toArray()[0];
  if (!existing) throw new Error("todo proposal persistence failed");
  // A replay cannot silently redefine an already presented approval or its choices.
  if (existing.todo_id !== (input.todo_id ?? null) || existing.workflow_id !== (input.workflow_id ?? null)
    || existing.title !== input.title || existing.context !== input.context
    || existing.source_label !== input.source_label || existing.source_url !== input.source_url
    || existing.choices !== JSON.stringify(input.choices)) throw new Error("todo_source_conflict");
  return { id: existing.id, status: existing.status, version: existing.version };
}
