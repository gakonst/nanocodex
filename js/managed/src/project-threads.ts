import type { NamedTool, ToolContext } from "nanocodex";
import { z } from "zod";

export const projectThreadInput = z.object({
  id: z.string().regex(/^[A-Za-z0-9_-]{1,64}$/),
  title: z.string().trim().min(1).max(160),
  input: z.string().min(1).max(65_536),
}).strict();
export type ProjectThreadInput = z.infer<typeof projectThreadInput>;
export type ProjectThread = {
  agent_id: string; parent_agent_id: string; project_root_id: string;
  origin_turn_id: string; turn_id: string; title: string; request_hash: string; created_at: number;
};
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export function initializeProjectThreads(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS project_threads (
    agent_id TEXT PRIMARY KEY, parent_agent_id TEXT NOT NULL, project_root_id TEXT NOT NULL,
    origin_turn_id TEXT NOT NULL, turn_id TEXT NOT NULL, title TEXT NOT NULL,
    request_hash TEXT NOT NULL, created_at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS project_threads_root ON project_threads(project_root_id, created_at);`);
}
/** Only the account's internal registry can establish project membership. */
export async function projectThreadRegistry(request: Request, storage: DurableObjectStorage): Promise<Response> {
  const url = new URL(request.url);
  const parent = url.pathname.split("/")[2] ?? "";
  if (!uuid.test(parent)) return Response.json({ error: "invalid_parent" }, { status: 400 });
  if (!storage.sql.exec("SELECT id FROM agent_registry WHERE id=? AND deleted_at IS NULL", parent).toArray().length)
    return Response.json({ error: "not_found" }, { status: 404 });
  const root = storage.sql.exec<{ project_root_id: string }>("SELECT project_root_id FROM project_threads WHERE agent_id=?", parent).toArray()[0]?.project_root_id ?? parent;
  if (request.method === "GET") {
    const rows = storage.sql.exec<ProjectThread>(`SELECT p.* FROM project_threads p JOIN agent_registry a ON a.id=p.agent_id
      WHERE p.project_root_id=? AND a.deleted_at IS NULL ORDER BY p.created_at, p.agent_id LIMIT 128`, root).toArray();
    return Response.json({ project_root_id: root, data: rows });
  }
  if (request.method !== "POST") return new Response(null, { status: 405 });
  const parsed = z.object({ agent_id: z.string().regex(uuid), origin_turn_id: z.string().max(128),
    turn_id: z.string().regex(/^[A-Za-z0-9._:-]{1,128}$/), title: z.string().min(1).max(160),
    request_hash: z.string().regex(/^[0-9a-f]{64}$/) }).strict().safeParse(await request.json());
  if (!parsed.success) return Response.json({ error: "invalid_request" }, { status: 400 });
  const body = parsed.data;
  if (body.agent_id === parent || body.agent_id === root
    || !storage.sql.exec("SELECT id FROM agent_registry WHERE id=? AND deleted_at IS NULL", body.agent_id).toArray().length)
    return Response.json({ error: "invalid_child" }, { status: 409 });
  const existing = storage.sql.exec<ProjectThread>("SELECT * FROM project_threads WHERE agent_id=?", body.agent_id).toArray()[0];
  if (existing) {
    if (existing.parent_agent_id !== parent || existing.project_root_id !== root || existing.request_hash !== body.request_hash
      || existing.title !== body.title || existing.turn_id !== body.turn_id)
      return Response.json({ error: "project_thread_conflict" }, { status: 409 });
    return Response.json(existing);
  }
  // An existing project root cannot be reparented into its descendants.
  if (storage.sql.exec("SELECT agent_id FROM project_threads WHERE project_root_id=? LIMIT 1", body.agent_id).toArray().length)
    return Response.json({ error: "project_thread_conflict" }, { status: 409 });
  if (storage.sql.exec<{ count: number }>(`SELECT COUNT(*) AS count FROM project_threads p JOIN agent_registry a ON a.id=p.agent_id
    WHERE p.project_root_id=? AND a.deleted_at IS NULL`, root).toArray()[0]!.count >= 128)
    return Response.json({ error: "project_thread_limit" }, { status: 409 });
  const row: ProjectThread = { ...body, parent_agent_id: parent, project_root_id: root, created_at: Date.now() };
  storage.sql.exec(`INSERT INTO project_threads (agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
    VALUES (?,?,?,?,?,?,?,?)`, row.agent_id, parent, root, row.origin_turn_id, row.turn_id, row.title, row.request_hash, row.created_at);
  return Response.json(row, { status: 201 });
}
/** Freeze creation configuration before the first cross-object write. */
export function retainProjectSpawn(storage: DurableObjectStorage, input: ProjectThreadInput, creation: string, originTurnId: string) {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS project_spawn_plans (
    id TEXT PRIMARY KEY, request_json TEXT NOT NULL, creation_json TEXT NOT NULL, origin_turn_id TEXT NOT NULL)`);
  const request = JSON.stringify(input);
  const previous = storage.sql.exec<{ request_json: string; creation_json: string; origin_turn_id: string }>(
    "SELECT request_json, creation_json, origin_turn_id FROM project_spawn_plans WHERE id=?", input.id).toArray()[0];
  if (previous) {
    if (previous.request_json !== request) throw new Error("project thread id conflicts with an earlier request");
    return { creation: previous.creation_json, originTurnId: previous.origin_turn_id };
  }
  storage.sql.exec("INSERT INTO project_spawn_plans(id,request_json,creation_json,origin_turn_id) VALUES (?,?,?,?)",
    input.id, request, creation, originTurnId);
  return { creation, originTurnId };
}

/** Retryable create/link/admit sequence. Each stage reuses durable identities. */
export async function spawnPersistentProjectThread(input: ProjectThreadInput, host: {
  sessionId: string; originTurnId: string;
  identity(key: string): Promise<string>;
  existing(agentId: string): Promise<ProjectThread | undefined>;
  create(key: string): Promise<void>;
  link(value: Omit<ProjectThread, "parent_agent_id" | "project_root_id" | "created_at">): Promise<ProjectThread>;
  admit(agentId: string, turnId: string, input: string): Promise<void>;
}): Promise<Omit<ProjectThread, "request_hash" | "created_at"> & { status: "accepted" }> {
  const key = `project:${host.sessionId}:${input.id}`;
  const agentId = await host.identity(key);
  const turnId = `project:${input.id}`;
  const requestHash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(input))))]
    .map(value => value.toString(16).padStart(2, "0")).join("");
  let row = await host.existing(agentId);
  if (row) {
    if (row.parent_agent_id !== host.sessionId || row.request_hash !== requestHash
      || row.title !== input.title || row.turn_id !== turnId) throw new Error("project thread retry conflicts with existing membership");
  } else {
    await host.create(key);
    row = await host.link({ agent_id: agentId, title: input.title, request_hash: requestHash,
      turn_id: turnId, origin_turn_id: host.originTurnId });
  }
  await host.admit(agentId, turnId, input.input);
  return { agent_id: agentId, project_root_id: row.project_root_id, parent_agent_id: row.parent_agent_id,
    origin_turn_id: row.origin_turn_id, turn_id: turnId, title: row.title, status: "accepted" };
}

/** Scope caller-chosen IDs to the sender, since unrelated conversations may reuse them. */
export function projectFollowupTurnId(senderId: string, id: string): string {
  return `project-followup:${senderId}:${id}`;
}

/** Account authorization is resolved by the host; project ancestry is deliberately irrelevant. */
export async function sendPersistentThreadFollowup(input: { agent_id: string; id: string; input: string }, host: {
  sessionId: string;
  resolve(agentId: string): Promise<string>;
  legacyTurnId(agentId: string, id: string): Promise<string | undefined>;
  admit(agentId: string, turnId: string, title: string, input: string): Promise<void>;
}): Promise<{ agent_id: string; turn_id: string; status: "accepted" }> {
  const title = await host.resolve(input.agent_id);
  const turnId = await host.legacyTurnId(input.agent_id, input.id) ?? projectFollowupTurnId(host.sessionId, input.id);
  await host.admit(input.agent_id, turnId, title, input.input);
  return { agent_id: input.agent_id, turn_id: turnId, status: "accepted" };
}

export function projectThreadTools(handlers: {
  spawn(input: ProjectThreadInput, context: ToolContext): Promise<unknown>;
  list(context: ToolContext): Promise<unknown>;
  read(agentId: string, context: ToolContext, turnId?: string): Promise<unknown>;
  send(input: { agent_id: string; id: string; input: string }, context: ToolContext): Promise<unknown>;
}): NamedTool[] {
  return [{
    name: "spawn_project_thread",
    description: "Start an independent persistent task thread in this project. It has its own managed agent and continues after this turn. Supply full task context; transcripts are not implicitly copied. Use a stable id: exact retries reuse the same thread and turn; changed input conflicts. Read results with read_project_thread and report them in the project chat. In-process spawn_agent remains available for short-lived delegation.",
    parameters: { type: "object", properties: {
      id: { type: "string", pattern: "^[A-Za-z0-9_-]{1,64}$" },
      title: { type: "string", minLength: 1, maxLength: 160 },
      input: { type: "string", minLength: 1, maxLength: 65536 },
    }, required: ["id", "title", "input"], additionalProperties: false },
    handler: (input, context) => handlers.spawn(projectThreadInput.parse(input), context),
  }, {
    name: "list_project_threads", description: "List this project's persistent task threads and their current turn outcomes. Threads are scoped to this account and project.",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    handler: (input, context) => { z.object({}).strict().parse(input); return handlers.list(context); },
  }, {
    name: "read_project_thread", description: "Read a persistent project thread's admitted task, execution state and final result. Threads in the current project and conversations this agent has sent follow-ups to are accessible. If still running, continue independent work before checking again.",
    parameters: { type: "object", properties: { agent_id: { type: "string" }, turn_id: { type: "string" } }, required: ["agent_id"], additionalProperties: false },
    handler: (input, context) => {
      const value = z.object({ agent_id: z.string().regex(uuid), turn_id: z.string().regex(/^[A-Za-z0-9._:-]{1,128}$/).optional() }).strict().parse(input);
      return handlers.read(value.agent_id, context, value.turn_id);
    },
  }, {
    name: "send_project_thread", description: "Send a follow-up to any accessible conversation in the same account, including parents, siblings and other projects. Use its agent_id from history search or project listings. Supply a stable id and explicit task input. Exact retries from this sender reuse the same turn; different input conflicts. Project membership and authority are unchanged. Outcomes return automatically to this conversation.",
    parameters: { type: "object", properties: { agent_id: { type: "string" }, id: { type: "string", pattern: "^[A-Za-z0-9_-]{1,64}$" }, input: { type: "string", minLength: 1, maxLength: 65536 } }, required: ["agent_id", "id", "input"], additionalProperties: false },
    handler: (input, context) => handlers.send(z.object({ agent_id: z.string().regex(uuid), id: projectThreadInput.shape.id, input: projectThreadInput.shape.input }).strict().parse(input), context),
  }];
}
