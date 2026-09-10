import { z } from "zod";

const name = z.string().regex(/^[A-Za-z0-9_-]{1,64}$/);
const file = z.object({
  path: z.string().max(512).refine(p => p.startsWith("/brain/") && !p.includes("\0")
    && p.slice(1).split("/").every(s => s !== "." && s !== ".." && s !== ""), "files must be inside /brain"),
  content: z.string().max(262_144),
}).strict();
export const networkSchema = z.discriminatedUnion("access", [
  z.object({ access: z.literal("enabled") }).strict(),
  z.object({ access: z.literal("disabled") }).strict(),
  z.object({ access: z.literal("restricted"), allowed_domains: z.array(z.string().max(253)
    .regex(/^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,63}$/)).min(1).max(100) }).strict(),
]);
export const environmentSchema = z.object({
  files: z.array(file).max(50).default([]),
  skills: z.array(z.object({ name, instructions: z.string().min(1).max(65_536) }).strict()).max(32).default([]),
  // Packages/skills are files and explicit setup commands in the selected shell.
  setup_commands: z.array(z.string().min(1).max(8192)).max(32).default([]),
  network: networkSchema.default({ access: "enabled" }),
}).strict();
export const configurationSchema = z.object({
  settings: z.object({
    model: z.enum(["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]),
    thinking: z.enum(["none", "low", "medium", "high", "xhigh", "max"]),
    reasoning_mode: z.enum(["standard", "pro"]), fast_mode: z.boolean(),
  }).strict().refine(s => s.model !== "gpt-6-astra" || s.thinking !== "none" && s.reasoning_mode !== "pro").optional(),
  instructions: z.string().max(65_536).optional(),
  tools: z.array(name).max(128).optional(),
  output_schema: z.record(z.string(), z.unknown()).optional(),
  prompt_cache: z.enum(["implicit", "explicit"]).optional(),
  environment: environmentSchema.optional(),
}).strict();
export type AgentConfiguration = z.infer<typeof configurationSchema>;
export type AgentEnvironment = z.infer<typeof environmentSchema>;
export type NetworkPolicy = z.infer<typeof networkSchema>;
export function parseConfiguration(value: unknown): AgentConfiguration {
  if (new TextEncoder().encode(JSON.stringify(value ?? {})).byteLength > 1_000_000) throw new TypeError("configuration exceeds 1 MB");
  return configurationSchema.parse(value ?? {});
}
export function networkAllows(policy: NetworkPolicy | undefined, value: string): boolean {
  if (!policy || policy.access === "enabled") return true;
  const url = new URL(value);
  return policy.access === "restricted" && ["http:", "https:"].includes(url.protocol)
    && !url.username && !url.password && !url.port && policy.allowed_domains.includes(url.hostname);
}

/** Account-owned immutable named templates. PUT retries are idempotent; use a new ID to revise. */
export async function configurationCatalog(request: Request, storage: DurableObjectStorage): Promise<Response> {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_configuration_catalog (
    kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL,
    PRIMARY KEY(kind, id))`);
  const url = new URL(request.url);
  const match = url.pathname.match(/^\/(agent-definitions|environment-templates)(?:\/([A-Za-z0-9_-]{1,64}))?$/);
  if (!match || url.search) return Response.json({ error: "invalid_request" }, { status: 400 });
  const [, kind, id] = match;
  if (request.method === "GET") {
    const rows = id ? storage.sql.exec<{ id: string; body: string; created_at: number }>(
      "SELECT id, body, created_at FROM managed_configuration_catalog WHERE kind = ? AND id = ?", kind!, id).toArray()
      : storage.sql.exec<{ id: string; body: string; created_at: number }>(
        "SELECT id, body, created_at FROM managed_configuration_catalog WHERE kind = ? ORDER BY id LIMIT 1001", kind!).toArray();
    const data = rows.map(({ body, ...row }) => ({ ...row, configuration: JSON.parse(body) }));
    return id ? data[0] ? Response.json(data[0]) : Response.json({ error: "not_found" }, { status: 404 })
      : Response.json({ data });
  }
  if (!id) return new Response(null, { status: 405 });
  if (request.method === "DELETE") {
    storage.sql.exec("DELETE FROM managed_configuration_catalog WHERE kind = ? AND id = ?", kind!, id);
    return new Response(null, { status: 204 });
  }
  if (request.method !== "PUT") return new Response(null, { status: 405 });
  try {
    const value = await request.json();
    const config = kind === "agent-definitions" ? parseConfiguration(value) : environmentSchema.parse(value);
    const body = JSON.stringify(config);
    if (new TextEncoder().encode(body).byteLength > 1_000_000) throw new TypeError("template exceeds 1 MB");
    const current = storage.sql.exec<{ body: string; created_at: number }>(
      "SELECT body, created_at FROM managed_configuration_catalog WHERE kind = ? AND id = ?", kind!, id).toArray()[0];
    if (current && current.body !== body) return Response.json({ error: "immutable_template" }, { status: 409 });
    if (!current && storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_configuration_catalog").one().n >= 1000)
      return Response.json({ error: "template_limit" }, { status: 409 });
    const createdAt = current?.created_at ?? Date.now();
    storage.sql.exec("INSERT OR IGNORE INTO managed_configuration_catalog VALUES (?, ?, ?, ?)", kind!, id, body, createdAt);
    return Response.json({ id, configuration: config, created_at: createdAt }, { status: current ? 200 : 201 });
  } catch { return Response.json({ error: "invalid_configuration" }, { status: 400 }); }
}
