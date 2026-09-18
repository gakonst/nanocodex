import { z } from "zod";
import type { Principal } from "./account-auth";

export const migrationOwner = "631f6a83-9e3f-474a-977a-68897d3ee436";
export const migrationTeam = "e1e0fc10-5e60-433d-b889-09c80dcd7c11";
export const migrationPath = "/v1/account/conversation-project-migration-20260918";
export function migrationAuthorized(principal: Principal | undefined): boolean {
  return !!principal && (principal.kind === "account_session" || principal.kind === "api_key")
    && !principal.connectGrant && principal.userId === migrationOwner && principal.teamId === migrationTeam
    && (["agents:read", "agents:write", "tools:use"] as const).every(cap => principal.capabilities.includes(cap));
}
export function initializeConversationProjects(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS conversation_projects (
    agent_id TEXT PRIMARY KEY, project_root_id TEXT NOT NULL, project_name TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS conversation_project_migration (
      id TEXT PRIMARY KEY, request_json TEXT NOT NULL, backup_json TEXT NOT NULL,
      receipt_json TEXT NOT NULL, created_at INTEGER NOT NULL);`);
}
const assignment = z.object({agent_id: z.string().uuid(), project_root_id: z.string().uuid(),
  project_name: z.enum(["Nanocodex", "DJBooth", "Personal Life"])}).strict();
const input = z.object({expected_snapshot: z.string().regex(/^[a-f0-9]{64}$/),
  assignments: z.array(assignment).min(3).max(10000)}).strict();
const migrationID = "conversation-projects-20260918";
function snapshot(storage: DurableObjectStorage) {
  return {
    agents: storage.sql.exec("SELECT id,title,created_at,updated_at,turn_count FROM agent_registry WHERE deleted_at IS NULL ORDER BY id").toArray(),
    execution_links: storage.sql.exec("SELECT * FROM project_threads ORDER BY agent_id").toArray(),
    assignments: storage.sql.exec("SELECT * FROM conversation_projects ORDER BY agent_id").toArray(),
  };
}
function membership(value: ReturnType<typeof snapshot>): string {
  return JSON.stringify({agents:value.agents.map(a => ({id:a.id,title:a.title})),
    execution_links:value.execution_links, assignments:value.assignments});
}
async function fingerprint(value: string): Promise<string> {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)))].map(b=>b.toString(16).padStart(2,"0")).join("");
}
/** Called only through the authenticated account router, using its owner-bound registry. */
export async function conversationProjectMigration(request: Request, storage: DurableObjectStorage): Promise<Response> {
  const json = (body: unknown, status=200) => Response.json(body, {status,headers:{"cache-control":"no-store"}});
  const prior = storage.sql.exec<{request_json:string;backup_json:string;receipt_json:string}>(
    "SELECT request_json,backup_json,receipt_json FROM conversation_project_migration WHERE id=?", migrationID).toArray()[0];
  if (request.method === "GET") {
    const current = snapshot(storage);
    return json({snapshot:current, snapshot_hash:await fingerprint(membership(current)),
      applied:prior ? JSON.parse(prior.receipt_json) : null,
      original_membership:prior ? JSON.parse(prior.backup_json) : null});
  }
  if (request.method !== "POST") return json({error:"method_not_allowed"},405);
  let raw: unknown;
  try { raw = await request.json(); } catch { return json({error:"invalid_request"},400); }
  const parsed = input.safeParse(raw);
  if (!parsed.success) return json({error:"invalid_request"},400);
  const body = {...parsed.data, assignments:parsed.data.assignments.sort((a,b)=>a.agent_id.localeCompare(b.agent_id))};
  const canonical = JSON.stringify(body);
  if (prior) return prior.request_json === canonical ? json(JSON.parse(prior.receipt_json)) : json({error:"migration_already_applied"},409);
  const before = snapshot(storage);
  const beforeMembership = membership(before);
  if (await fingerprint(beforeMembership) !== body.expected_snapshot) return json({error:"snapshot_changed"},409);
  const ids = before.agents.map(a=>String(a.id));
  const rows = new Map(body.assignments.map(a=>[a.agent_id,a]));
  if (rows.size !== body.assignments.length || rows.size !== ids.length || ids.some(id=>!rows.has(id)))
    return json({error:"inventory_mismatch"},409);
  const roots = new Map<string,string>();
  for (const row of rows.values()) {
    const root = rows.get(row.project_root_id);
    if (!root || root.agent_id !== root.project_root_id || root.project_name !== row.project_name)
      return json({error:"invalid_project_root"},400);
    const existing = roots.get(row.project_name);
    if (existing && existing !== row.project_root_id) return json({error:"duplicate_project_name"},400);
    roots.set(row.project_name,row.project_root_id);
  }
  if (roots.size !== 3) return json({error:"three_projects_required"},400);
  return storage.transactionSync(() => {
    if (membership(snapshot(storage)) !== beforeMembership) return json({error:"snapshot_changed"},409);
    const concurrent = storage.sql.exec("SELECT id FROM conversation_project_migration WHERE id=?",migrationID).toArray();
    if (concurrent.length) return json({error:"migration_already_applied"},409);
    const counts: Record<string,number> = {};
    for (const row of rows.values()) {
      storage.sql.exec("INSERT INTO conversation_projects(agent_id,project_root_id,project_name) VALUES (?,?,?) ON CONFLICT(agent_id) DO UPDATE SET project_root_id=excluded.project_root_id,project_name=excluded.project_name",
        row.agent_id,row.project_root_id,row.project_name);
      counts[row.project_name]=(counts[row.project_name] ?? 0)+1;
    }
    const after = snapshot(storage);
    if (JSON.stringify(before.execution_links) !== JSON.stringify(after.execution_links) || before.agents.length !== after.agents.length)
      throw new Error("migration invariant failed");
    const receipt = {id:migrationID,account_owner_id:migrationOwner,team_id:migrationTeam,
      total:ids.length,counts,projects:Object.fromEntries(roots),execution_links_preserved:before.execution_links.length,
      transcript_mutations:0,created_at:Date.now()};
    storage.sql.exec("INSERT INTO conversation_project_migration(id,request_json,backup_json,receipt_json,created_at) VALUES (?,?,?,?,?)",
      migrationID,canonical,JSON.stringify(before),JSON.stringify(receipt),receipt.created_at);
    return json(receipt,201);
  });
}

