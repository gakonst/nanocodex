import { createHmac, randomBytes, createHash } from "node:crypto";
import type { Workspace } from "nanocodex-tools";
import type { DurableEvent } from "./durable-events";

const lifecycle = new Set(["agent_created", "turn_accepted", "turn_completed", "turn_failed", "turn_cancelled", "stream_failed"]);
type Delivery = { id: string; body: string; attempt: number; retry_at: number; status: string };

/** Session-owned operational projections. Receipts and outbox writes share the event transaction. */
export class SessionOperations {
  constructor(readonly storage: DurableObjectStorage) {
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_configuration (singleton INTEGER PRIMARY KEY CHECK(singleton=1), body TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS managed_environment_setup (singleton INTEGER PRIMARY KEY CHECK(singleton=1), state TEXT NOT NULL, step INTEGER NOT NULL, error TEXT);
      CREATE TABLE IF NOT EXISTS managed_webhook (singleton INTEGER PRIMARY KEY CHECK(singleton=1), url TEXT NOT NULL, secret TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS managed_webhook_deliveries (id TEXT PRIMARY KEY, body TEXT NOT NULL, attempt INTEGER NOT NULL, retry_at INTEGER NOT NULL, status TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS managed_model_usage (id TEXT PRIMARY KEY, cursor INTEGER NOT NULL, agent_id TEXT NOT NULL, turn_id TEXT, type TEXT NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS managed_model_usage_cursor ON managed_model_usage(cursor);
      CREATE INDEX IF NOT EXISTS managed_model_usage_agent_cursor ON managed_model_usage(agent_id,cursor);
      CREATE TABLE IF NOT EXISTS managed_turn_usage (cursor INTEGER PRIMARY KEY, turn_id TEXT, created_at INTEGER NOT NULL, type TEXT NOT NULL, usage TEXT);
      CREATE TABLE IF NOT EXISTS managed_artifacts (id TEXT PRIMARY KEY, turn_id TEXT NOT NULL, path TEXT NOT NULL, digest TEXT NOT NULL, size INTEGER NOT NULL, created_at INTEGER NOT NULL, body BLOB NOT NULL, UNIQUE(turn_id,path));
      CREATE TABLE IF NOT EXISTS managed_artifact_publications (turn_id TEXT PRIMARY KEY, state TEXT NOT NULL, error TEXT);
    `);
    storage.sql.exec("UPDATE managed_environment_setup SET state='failed', error='Setup was interrupted; recreate the session to retry.' WHERE state='running'");
  }
  record(event: DurableEvent<{ type: string; usage?: unknown }>, agentId?: string): void {
    if (event.message.type === "event") {
      const message = event.message as { type: string; agent_id?: number; event?: { type: string; request_id: string; seq: number; payload: Record<string, unknown> } };
      const inner = message.event;
      if (inner && ["model.call.completed", "model.warmup.completed", "model.compaction.completed"].includes(inner.type)) {
        const id = typeof inner.payload.response_id === "string" ? inner.payload.response_id : `${inner.request_id}:${inner.seq}`;
        this.storage.sql.exec("INSERT OR IGNORE INTO managed_model_usage VALUES (?, ?, ?, ?, ?, ?, ?)", id, event.cursor,
          message.agent_id === undefined ? "root" : String(message.agent_id), event.turn_id, inner.type, event.created_at, JSON.stringify(inner.payload));
      }
    }
    if (!lifecycle.has(event.message.type)) return;
    if (event.turn_id && event.message.type.startsWith("turn_")) this.storage.sql.exec(
      "INSERT OR IGNORE INTO managed_turn_usage VALUES (?, ?, ?, ?, ?)", event.cursor, event.turn_id,
      event.created_at, event.message.type, event.message.usage == null ? null : JSON.stringify(event.message.usage));
    if (!agentId || !this.storage.sql.exec("SELECT singleton FROM managed_webhook").toArray().length) return;
    const id = `${agentId}:${event.cursor}`;
    this.storage.sql.exec("INSERT OR IGNORE INTO managed_webhook_deliveries VALUES (?, ?, 0, 0, 'pending')", id,
      JSON.stringify({ id, type: event.message.type, agent_id: agentId, turn_id: event.turn_id, cursor: event.cursor, created_at: event.created_at }));
  }
  nextAlarm(): number | undefined {
    return this.storage.sql.exec<{ retry_at: number }>("SELECT retry_at FROM managed_webhook_deliveries WHERE status='pending' ORDER BY retry_at LIMIT 1").toArray()[0]?.retry_at;
  }
  async drain(fetcher: typeof fetch = fetch): Promise<void> {
    const hook = this.storage.sql.exec<{ url: string; secret: string }>("SELECT url, secret FROM managed_webhook").toArray()[0];
    if (!hook) return;
    for (const row of this.storage.sql.exec<Delivery>("SELECT * FROM managed_webhook_deliveries WHERE status='pending' AND retry_at <= ? ORDER BY retry_at LIMIT 8", Date.now()).toArray()) {
      const timestamp = String(Math.floor(Date.now() / 1000));
      const signature = createHmac("sha256", hook.secret).update(`${row.id}.${timestamp}.${row.body}`).digest("hex");
      let ok = false;
      try {
        const response = await fetcher(hook.url, { method: "POST", body: row.body, redirect: "manual", signal: AbortSignal.timeout(10_000), headers: {
          "content-type": "application/json", "webhook-id": row.id, "webhook-timestamp": timestamp, "webhook-signature": `v1,${signature}`,
        } });
        ok = response.ok; await response.body?.cancel();
      } catch { /* Durable bounded retry; response bodies never enter the outbox. */ }
      this.storage.sql.exec("UPDATE managed_webhook_deliveries SET attempt=?, retry_at=?, status=? WHERE id=? AND status='pending'",
        row.attempt + 1, Date.now() + Math.min(3_600_000, 1000 * 2 ** row.attempt), ok ? "delivered" : row.attempt >= 11 ? "failed" : "pending", row.id);
    }
    this.storage.sql.exec("DELETE FROM managed_webhook_deliveries WHERE id IN (SELECT id FROM managed_webhook_deliveries WHERE status='delivered' ORDER BY retry_at DESC LIMIT -1 OFFSET 1000)");
  }
  async webhook(request: Request): Promise<Response> {
    if (request.method === "GET") return Response.json({
      endpoint: this.storage.sql.exec("SELECT url FROM managed_webhook").toArray()[0] ?? null,
      deliveries: this.storage.sql.exec("SELECT id, attempt, retry_at, status FROM managed_webhook_deliveries ORDER BY retry_at DESC LIMIT 100").toArray(),
    });
    if (request.method === "DELETE") {
      this.storage.transactionSync(() => {
        this.storage.sql.exec("DELETE FROM managed_webhook"); this.storage.sql.exec("DELETE FROM managed_webhook_deliveries");
      });
      return new Response(null, { status: 204 });
    }
    if (request.method !== "PUT") return new Response(null, { status: 405 });
    try {
      const body = await request.json<{ url: string }>();
      const url = new URL(body.url);
      if (Object.keys(body).length !== 1 || url.protocol !== "https:" || url.username || url.password || url.port || url.hash
        || !/^(?:[a-z0-9-]+\.)+[a-z]{2,63}$/.test(url.hostname) || url.hostname.endsWith(".localhost")) throw new Error();
      if (this.storage.sql.exec("SELECT singleton FROM managed_webhook").toArray().length)
        return Response.json({ error: "webhook_exists", message: "Delete the endpoint before replacing it." }, { status: 409 });
      const secret = Array.from(randomBytes(32), byte => byte.toString(16).padStart(2, "0")).join("");
      this.storage.sql.exec("INSERT INTO managed_webhook VALUES (1, ?, ?)", url.href, secret);
      return Response.json({ url: url.href, secret }, { status: 201, headers: { "cache-control": "no-store" } });
    } catch { return Response.json({ error: "invalid_webhook" }, { status: 400 }); }
  }
  async publish(turnId: string, workspace: Workspace, isActive: () => boolean = () => true): Promise<void> {
    if (this.storage.sql.exec("SELECT turn_id FROM managed_artifact_publications WHERE turn_id=?", turnId).toArray().length) return;
    try {
      const all = await workspace.list("/brain/outputs", { recursive: true, maxEntries: 100 }).catch(error => {
        if (error?.code === "ENOENT") return [];
        throw error;
      });
      const files = all.filter(f => f.kind === "file" && f.path.startsWith("/brain/outputs/"));
      if (files.length > 50 || files.some(f => (f.size ?? Infinity) > 1_000_000)) throw new Error("publication allows 50 files, up to 1 MB each");
      let total = 0;
      const captured: { path: string; bytes: Uint8Array; digest: string }[] = [];
      for (const file of files) {
        const bytes = await workspace.readFile(file.path); total += bytes.byteLength;
        if (bytes.byteLength > 1_000_000 || total > 10_000_000) throw new Error("publication exceeds 10 MB");
        captured.push({ path: file.path, bytes, digest: createHash("sha256").update(bytes).digest("hex") });
      }
      if (!isActive()) return;
      this.storage.transactionSync(() => {
        for (const file of captured) this.storage.sql.exec("INSERT OR IGNORE INTO managed_artifacts VALUES (?, ?, ?, ?, ?, ?, ?)",
          createHash("sha256").update(`${turnId}\0${file.path}`).digest("hex"), turnId, file.path, file.digest, file.bytes.byteLength, Date.now(), file.bytes);
        this.storage.sql.exec("INSERT OR IGNORE INTO managed_artifact_publications VALUES (?, 'ready', NULL)", turnId);
      });
    } catch (error) {
      if (!isActive()) return;
      this.storage.sql.exec("INSERT OR IGNORE INTO managed_artifact_publications VALUES (?, 'failed', ?)", turnId, error instanceof Error ? error.message : "publication failed");
    }
  }
  artifacts(request: Request): Response {
    const url = new URL(request.url);
    const id = url.pathname.match(/^\/artifacts\/([a-f0-9]{64})\/content$/)?.[1];
    if (request.method !== "GET") return new Response(null, { status: 405 });
    if (id) {
      const row = this.storage.sql.exec<{ body: ArrayBuffer; digest: string; size: number }>("SELECT body,digest,size FROM managed_artifacts WHERE id=?", id).toArray()[0];
      return row ? new Response(row.body, { headers: { "content-type": "application/octet-stream", "content-disposition": "attachment", "x-content-type-options": "nosniff", etag: `"${row.digest}"`, "cache-control": "private, no-store" } })
        : Response.json({ error: "not_found" }, { status: 404 });
    }
    if (url.pathname !== "/artifacts") return new Response(null, { status: 404 });
    const turn = url.searchParams.get("turn_id");
    return Response.json({ data: turn ? this.storage.sql.exec("SELECT id,turn_id,path,digest,size,created_at FROM managed_artifacts WHERE turn_id=? ORDER BY path", turn).toArray()
      : this.storage.sql.exec("SELECT id,turn_id,path,digest,size,created_at FROM managed_artifacts ORDER BY created_at DESC LIMIT 256").toArray(),
      publications: this.storage.sql.exec("SELECT turn_id,state,error FROM managed_artifact_publications ORDER BY rowid DESC LIMIT 256").toArray() });
  }
  requests(after: string, agentId: string | null): Response {
    if (!/^(0|[1-9][0-9]{0,17})$/.test(after) || agentId !== null && !/^(root|[0-9]{1,20})$/.test(agentId)) return Response.json({ error: "invalid_cursor_or_agent" }, { status: 400 });
    const rows = this.storage.sql.exec<{ id: string; cursor: number; agent_id: string; turn_id: string | null; type: string; created_at: number; payload: string }>(
      `SELECT * FROM managed_model_usage WHERE cursor > ?${agentId === null ? "" : " AND agent_id=?"} ORDER BY cursor LIMIT 257`, after, ...(agentId === null ? [] : [agentId])).toArray();
    return Response.json({ data: rows.slice(0,256).map(({ payload, ...row }) => ({ ...row, cursor: String(row.cursor), payload: JSON.parse(payload) })), has_more: rows.length > 256 });
  }
  usage(after: string): Response {
    if (!/^(0|[1-9][0-9]{0,17})$/.test(after)) return Response.json({ error: "invalid_cursor" }, { status: 400 });
    const rows = this.storage.sql.exec<{ cursor: number; turn_id: string; created_at: number; type: string; usage: string | null }>(
      "SELECT * FROM managed_turn_usage WHERE cursor > ? ORDER BY cursor LIMIT 257", after).toArray();
    return Response.json({ data: rows.slice(0,256).map(r => ({ ...r, cursor: String(r.cursor), usage: r.usage === null ? null : JSON.parse(r.usage) })), has_more: rows.length > 256 });
  }
}
