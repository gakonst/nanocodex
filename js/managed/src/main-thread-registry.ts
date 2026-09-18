import { z } from "zod";

const id = z.string().regex(/^[A-Za-z0-9_-]{1,64}$/);
const mainInput = z.object({ agent_id: id }).strict();
const projectInput = z.object({ name: z.string().trim().min(1).max(160), coordinator_agent_id: id }).strict();
export type CanonicalProject = { id: string; name: string; coordinator_agent_id: string };
const error = (value: string, status = 409) => Response.json({ error: value }, { status });

export function initializeMainThreadRegistry(storage: DurableObjectStorage): void {
  const columns = storage.sql.exec<{ name: string }>("PRAGMA table_info(agent_registry)").toArray();
  if (!columns.some(column => column.name === "team_id")) storage.sql.exec("ALTER TABLE agent_registry ADD COLUMN team_id TEXT");
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS main_threads (
    team_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL UNIQUE);
    CREATE TABLE IF NOT EXISTS canonical_projects (
    team_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL,
    coordinator_agent_id TEXT NOT NULL UNIQUE, PRIMARY KEY(team_id,id));`);
}

/** Project discovery is a read-only view; legacy team metadata is never adopted here. */
function projectCandidates(storage: DurableObjectStorage, team: string, verifiedLegacy: boolean): CanonicalProject[] {
  const eligible = `JOIN agent_registry a ON a.id=p.coordinator_agent_id
    WHERE (a.team_id=? OR (a.team_id IS NULL AND ?)) AND a.deleted_at IS NULL
    AND NOT EXISTS (SELECT 1 FROM project_threads t WHERE t.agent_id=a.id)
    AND NOT EXISTS (SELECT 1 FROM main_threads m WHERE m.agent_id=a.id)`;
  const canonical = storage.sql.exec<CanonicalProject>(`SELECT p.id,p.name,p.coordinator_agent_id
    FROM canonical_projects p ${eligible} AND p.team_id=? ORDER BY p.id`, team, verifiedLegacy ? 1 : 0, team).toArray();
  const projected = verifiedLegacy ? storage.sql.exec<CanonicalProject>(`SELECT p.id,p.name,p.coordinator_agent_id FROM
    (SELECT 'project-' || agent_id AS id, project_name AS name, agent_id AS coordinator_agent_id
      FROM conversation_projects WHERE agent_id=project_root_id) p ${eligible} ORDER BY p.id`, team, verifiedLegacy ? 1 : 0).toArray()
    .filter(row => z.string().uuid().safeParse(row.coordinator_agent_id).success) : [];
  return [...canonical, ...projected];
}

/** The account DO supplies ownership; registration metadata supplies exact team scope. */
export async function mainThreadRegistry(
  request: Request,
  storage: DurableObjectStorage,
  validateIdentity?: (agentId: string, teamId: string) => Promise<boolean>,
): Promise<Response> {
  const url = new URL(request.url);
  const team = id.safeParse(url.searchParams.get("team_id"));
  if (!team.success) return error("invalid_team", 400);
  const teamId = team.data;
  if (url.pathname.startsWith("/canonical-role/")) {
    if (request.method !== "GET") return error("method_not_allowed", 405);
    const agent = url.pathname.slice("/canonical-role/".length);
    if (!id.safeParse(agent).success) return error("not_found", 404);
    const verified = new Set<string>();
    const membership = () => storage.sql.exec<{ project_root_id: string; parent_agent_id: string }>(
      "SELECT project_root_id,parent_agent_id FROM project_threads WHERE agent_id=?", agent).toArray()[0];
    const initialTask = membership();
    if (validateIdentity) {
      try {
        for (const candidate of new Set([agent, ...(initialTask ? [initialTask.project_root_id] : [])])) {
          if (!await validateIdentity(candidate, teamId)) return error("not_found", 404);
          verified.add(candidate);
        }
      } catch { return error("identity_validation_unavailable", 503); }
    }
    // No awaits below: re-read active scope and execution membership after live identity I/O.
    const active = (agentId: string) => storage.sql.exec(
      "SELECT id FROM agent_registry WHERE id=? AND (team_id=? OR (team_id IS NULL AND ?)) AND deleted_at IS NULL",
      agentId, teamId, verified.has(agentId) ? 1 : 0).toArray().length > 0;
    const foreign = (agentId: string) => storage.sql.exec(
      "SELECT agent_id FROM main_threads WHERE agent_id=? AND team_id<>?", agentId, teamId).toArray().length > 0
      || storage.sql.exec("SELECT id FROM canonical_projects WHERE coordinator_agent_id=? AND team_id<>?", agentId, teamId).toArray().length > 0;
    if (!active(agent) || foreign(agent)) return error("not_found", 404);
    const task = membership();
    if (task?.project_root_id !== initialTask?.project_root_id || task?.parent_agent_id !== initialTask?.parent_agent_id)
      return error("not_found", 404);
    if (storage.sql.exec("SELECT agent_id FROM main_threads WHERE agent_id=? AND team_id=?", agent, teamId).toArray().length)
      return Response.json({ role: "main" });
    // Match discovery's canonical-first coordinator/ID deduplication. Only an
    // individually verified navigation root can supply a projected identity.
    const ids = new Set<string>(), coordinators = new Set<string>();
    const projects = projectCandidates(storage, teamId, !!validateIdentity).filter(project => {
      if (ids.has(project.id) || coordinators.has(project.coordinator_agent_id)) return false;
      ids.add(project.id); coordinators.add(project.coordinator_agent_id); return true;
    });
    const project = projects.find(project => project.coordinator_agent_id === agent);
    if (project) return Response.json({ role: "project_coordinator", project_id: project.id, project_root_id: agent });
    if (task && active(task.project_root_id)) {
      if (foreign(task.project_root_id)) return error("not_found", 404);
      const rootProject = projects.find(project => project.coordinator_agent_id === task.project_root_id);
      return Response.json({ role: "project_task",
        ...(rootProject ? { project_id: rootProject.id } : {}), project_root_id: task.project_root_id,
        ...(active(task.parent_agent_id) ? { parent_agent_id: task.parent_agent_id } : {}),
      });
    }
    return Response.json({ role: "conversation" });
  }
  const main = url.pathname === "/main-thread";
  const projectId = url.pathname.startsWith("/projects/") ? url.pathname.slice("/projects/".length) : undefined;
  if (!main && url.pathname !== "/projects" && (!projectId || !id.safeParse(projectId).success)) return error("invalid_project", 400);
  if (request.method === "GET") {
    if (main) {
      const row = storage.sql.exec<{ agent_id: string; registered_id: string | null; team_id: string | null; deleted_at: number | null }>(`SELECT m.agent_id,a.id AS registered_id,a.team_id,a.deleted_at FROM main_threads m
        LEFT JOIN agent_registry a ON a.id=m.agent_id WHERE m.team_id=?`, teamId).toArray()[0];
      if (!row) return error("not_found", 404);
      if (row.registered_id && row.team_id !== teamId) return error("not_found", 404);
      if (!row.registered_id || row.deleted_at !== null) return error("main_thread_deleted", 410);
      return Response.json({ agent_id: row.agent_id });
    }
    if (projectId) return error("method_not_allowed", 405);
    const approved = new Set<string>();
    for (const row of projectCandidates(storage, teamId, !!validateIdentity)) {
      if (approved.has(row.coordinator_agent_id)) continue;
      try {
        if (!validateIdentity || await validateIdentity(row.coordinator_agent_id, teamId)) approved.add(row.coordinator_agent_id);
      } catch { return error("identity_validation_unavailable", 503); }
    }
    // Identity validation yields: re-read active state, membership and metadata afterwards.
    const ids = new Set<string>(), coordinators = new Set<string>();
    const data = projectCandidates(storage, teamId, !!validateIdentity).filter(row => {
      if (!approved.has(row.coordinator_agent_id) || ids.has(row.id) || coordinators.has(row.coordinator_agent_id)) return false;
      ids.add(row.id); coordinators.add(row.coordinator_agent_id); return true;
    }).sort((a, b) => a.id.localeCompare(b.id));
    return Response.json({ data });
  }
  if (request.method !== "PUT" || (!main && !projectId)) return error("method_not_allowed", 405);
  let body: unknown;
  try { body = await request.json(); } catch { return error("invalid_request", 400); }
  const parsed = main ? mainInput.safeParse(body) : projectInput.safeParse(body);
  if (!parsed.success) return error("invalid_request", 400);
  const agent = "agent_id" in parsed.data ? parsed.data.agent_id : parsed.data.coordinator_agent_id;
  if (validateIdentity) {
    try {
      if (!await validateIdentity(agent, teamId)) return error("agent_not_found", 404);
    } catch { return error("identity_validation_unavailable", 503); }
  }
  // Re-read every local constraint after identity I/O; no awaits before mutation.
  const owned = storage.sql.exec<{ team_id: string | null }>(
    "SELECT team_id FROM agent_registry WHERE id=? AND deleted_at IS NULL", agent).toArray()[0];
  if (!owned || (owned.team_id !== teamId && !(owned.team_id === null && validateIdentity)))
    return error("agent_not_found", 404);
  // Only successful session RPC verification may establish a legacy row's team.
  const retainVerifiedTeam = () => {
    if (owned.team_id === null) storage.sql.exec(
      "UPDATE agent_registry SET team_id=? WHERE id=? AND team_id IS NULL AND deleted_at IS NULL", teamId, agent);
  };
  if (storage.sql.exec("SELECT agent_id FROM project_threads WHERE agent_id=?", agent).toArray().length)
    return error("project_thread_conflict");
  if (main) {
    if (storage.sql.exec("SELECT id FROM canonical_projects WHERE coordinator_agent_id=?", agent).toArray().length
      || storage.sql.exec("SELECT agent_id FROM project_threads WHERE project_root_id=? OR parent_agent_id=? LIMIT 1", agent, agent).toArray().length)
      return error("project_thread_conflict");
    const existing = storage.sql.exec<{ agent_id: string }>("SELECT agent_id FROM main_threads WHERE team_id=?", teamId).toArray()[0];
    if (existing && existing.agent_id !== agent) return error("main_thread_conflict");
    retainVerifiedTeam();
    if (!existing) storage.sql.exec("INSERT INTO main_threads(team_id,agent_id) VALUES (?,?)", teamId, agent);
    return Response.json({ agent_id: agent }, { status: existing ? 200 : 201 });
  }
  if (storage.sql.exec("SELECT agent_id FROM main_threads WHERE agent_id=?", agent).toArray().length) return error("main_thread_conflict");
  const existing = storage.sql.exec<CanonicalProject>("SELECT id,name,coordinator_agent_id FROM canonical_projects WHERE team_id=? AND id=?", teamId, projectId!).toArray()[0];
  // A migrated root's stable ID cannot be claimed by a different coordinator,
  // even if its projection is currently hidden by ownership or active-state checks.
  const projectedRoot = projectId!.startsWith("project-") ? projectId!.slice("project-".length) : undefined;
  if (!existing && projectedRoot && storage.sql.exec(
    "SELECT agent_id FROM conversation_projects WHERE agent_id=? AND project_root_id=agent_id", projectedRoot).toArray().length
    && projectedRoot !== agent) return error("project_conflict");
  if (existing && existing.coordinator_agent_id !== agent) return error("project_conflict");
  const assigned = storage.sql.exec<{ team_id: string; id: string }>("SELECT team_id,id FROM canonical_projects WHERE coordinator_agent_id=?", agent).toArray()[0];
  if (assigned && (assigned.team_id !== teamId || assigned.id !== projectId)) return error("project_conflict");
  retainVerifiedTeam();
  const name = (parsed.data as z.infer<typeof projectInput>).name;
  storage.sql.exec(`INSERT INTO canonical_projects(team_id,id,name,coordinator_agent_id) VALUES (?,?,?,?)
    ON CONFLICT(team_id,id) DO UPDATE SET name=excluded.name`, teamId, projectId!, name, agent);
  return Response.json({ id: projectId, name, coordinator_agent_id: agent }, { status: existing ? 200 : 201 });
}

/** Keep established main/coordinator identities from becoming children later. */
export async function mainThreadMembershipGuard(request: Request, storage: DurableObjectStorage): Promise<Response | undefined> {
  const url = new URL(request.url);
  const parent = url.pathname.split("/")[2]!;
  if (request.method === "GET" && url.searchParams.get("spawn_preflight") === "1") {
    if (storage.sql.exec("SELECT agent_id FROM main_threads WHERE agent_id=?", parent).toArray().length)
      return error("main_thread_conflict");
    return;
  }
  if (request.method !== "POST") return;
  let body: { agent_id?: unknown };
  try { body = await request.clone().json(); } catch { return error("invalid_request", 400); }
  if (storage.sql.exec("SELECT agent_id FROM main_threads WHERE agent_id=?", parent).toArray().length) return error("main_thread_conflict");
  if (typeof body?.agent_id !== "string") return error("invalid_request", 400);
  if (storage.sql.exec("SELECT agent_id FROM main_threads WHERE agent_id=?", body.agent_id).toArray().length
    || storage.sql.exec("SELECT id FROM canonical_projects WHERE coordinator_agent_id=?", body.agent_id).toArray().length)
    return error("project_thread_conflict");
}
