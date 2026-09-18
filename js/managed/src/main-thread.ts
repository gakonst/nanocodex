import type { NamedTool, ToolContext } from "nanocodex";
import { z } from "zod";

export const canonicalProjectInput = z.object({
  name: z.string().trim().min(1).max(160),
  coordinator_agent_id: z.string().uuid().optional(),
}).strict();
export const canonicalProjectId = z.string().regex(/^[A-Za-z0-9_-]{1,64}$/);
export type CanonicalProject = { id: string; name: string; coordinator_agent_id: string };

/** Public protocol; account/team selection comes exclusively from the authenticated principal. */
export async function mainThreadRequest(request: Request, host: {
  teamId: string;
  registry(path: string, init?: RequestInit): Promise<Response>;
  create(key: string): Promise<Response>;
}): Promise<Response> {
  const url = new URL(request.url);
  if (url.search) return Response.json({ error: "invalid_request" }, { status: 400 });
  const path = url.pathname.slice(3);
  const main = path === "/main-thread";
  const projectId = path.startsWith("/projects/") ? path.slice(10) : undefined;
  if (!main && path !== "/projects" && !canonicalProjectId.safeParse(projectId).success)
    return Response.json({ error: "invalid_project_id" }, { status: 400 });
  if (request.method === "GET" && (main || path === "/projects")) return host.registry(path);
  if (request.method !== "PUT" || (!main && !projectId)) return new Response(null, { status: 405 });
  let body: z.infer<typeof canonicalProjectInput> | undefined;
  try {
    const text = await request.text();
    const value = text ? JSON.parse(text) : {};
    if (main) z.object({}).strict().parse(value);
    else body = canonicalProjectInput.parse(value);
  } catch { return Response.json({ error: "invalid_request" }, { status: 400 }); }
  const existing = await host.registry(main ? "/main-thread" : "/projects");
  let agentId: string | undefined;
  if (main) {
    if (existing.ok || existing.status !== 404) return existing;
  } else {
    if (!existing.ok) return existing;
    const row = (await existing.json<{ data: CanonicalProject[] }>()).data.find(row => row.id === projectId);
    if (row && body!.coordinator_agent_id && row.coordinator_agent_id !== body!.coordinator_agent_id)
      return Response.json({ error: "project_conflict" }, { status: 409 });
    agentId = row?.coordinator_agent_id ?? body!.coordinator_agent_id;
  }
  if (!agentId) {
    const identity = await host.registry(`/canonical-generations/${main ? "main" : `projects/${projectId}`}`);
    if (!identity.ok) return identity;
    const { generation } = await identity.json<{ generation: number }>();
    if (!Number.isSafeInteger(generation) || generation < 0)
      return Response.json({ error: "invalid_canonical_generation" }, { status: 503 });
    const key = `canonical:${host.teamId}:${main ? "main" : `project:${projectId}`}`;
    const created = await host.create(generation === 0 ? key : `${key}:generation:${generation}`);
    if (!created.ok) return created;
    agentId = (await created.json<{ agent_id: string }>()).agent_id;
  }
  return host.registry(path, { method: "PUT", headers: { "content-type": "application/json" },
    body: JSON.stringify(main ? { agent_id: agentId } : { ...body, coordinator_agent_id: agentId }) });
}

/** Freeze routing intent before creating a coordinator or admitting any turn. */
export function retainMainRoute(storage: DurableObjectStorage, input: { project_id: string; name: string; id: string; input: string }, creation = "{}"): { creation: string } {
  storage.sql.exec("CREATE TABLE IF NOT EXISTS main_route_plans (id TEXT PRIMARY KEY, request_json TEXT NOT NULL)");
  const value = JSON.stringify(input);
  const previous = storage.sql.exec<{ request_json: string }>("SELECT request_json FROM main_route_plans WHERE id=?", input.id).toArray()[0];
  if (previous && previous.request_json !== value) throw new Error("project route id conflicts with an earlier request");
  if (!previous) storage.sql.exec("INSERT INTO main_route_plans(id,request_json) VALUES (?,?)", input.id, value);
  // Separate table also upgrades retained routes created before snapshots existed.
  storage.sql.exec("CREATE TABLE IF NOT EXISTS main_route_creations (id TEXT PRIMARY KEY, creation_json TEXT NOT NULL)");
  storage.sql.exec("INSERT OR IGNORE INTO main_route_creations(id,creation_json) VALUES (?,?)", input.id, creation);
  return { creation: storage.sql.exec<{ creation_json: string }>(
    "SELECT creation_json FROM main_route_creations WHERE id=?", input.id).toArray()[0]!.creation_json };
}

export function mainThreadTools(handlers: {
  list(context: ToolContext): Promise<unknown>;
  read(id: string, turnId: string | undefined, context: ToolContext): Promise<unknown>;
  route(input: { project_id: string; name: string; id: string; input: string }, context: ToolContext): Promise<unknown>;
}): NamedTool[] {
  return [{
    name: "list_projects", description: "List canonical projects for this Main Thread's account and team. Reuse their coordinators for related work.",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    handler: (input, context) => { z.object({}).strict().parse(input); return handlers.list(context); },
  }, {
    name: "read_project", description: "Read a canonical project's coordinator state or an exact coordinator turn outcome. Available only in Main Thread.",
    parameters: { type: "object", properties: { project_id: { type: "string" }, turn_id: { type: "string" } }, required: ["project_id"], additionalProperties: false },
    handler: (input, context) => { const value = z.object({ project_id: canonicalProjectId, turn_id: z.string().regex(/^[A-Za-z0-9._:-]{1,128}$/).optional() }).strict().parse(input); return handlers.read(value.project_id, value.turn_id, context); },
  }, {
    name: "route_project", description: "Route work from Main Thread to a canonical project coordinator, creating the project if missing. Reuse project_id and a stable request id. Exact retries reuse the turn; changed input conflicts. Initial and later delegated outcomes return automatically to Main. Supply all needed context.",
    parameters: { type: "object", properties: { project_id: { type: "string" }, name: { type: "string" }, id: { type: "string" }, input: { type: "string" } }, required: ["project_id", "name", "id", "input"], additionalProperties: false },
    handler: (input, context) => handlers.route(z.object({ project_id: canonicalProjectId, name: canonicalProjectInput.shape.name, id: canonicalProjectId, input: z.string().min(1).max(65536) }).strict().parse(input), context),
  }];
}
