import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DiagnosticJournal } from "../src/diagnostic-journal";

it("migrates lease/connection indexes to partial indexes and still pages connection evidence", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const sql = ctx.storage.sql;
    // Schema written by earlier deployments: full indexes on mostly-NULL columns.
    sql.exec(`CREATE TABLE diagnostic_events (seq INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, thread_id TEXT,
      lease_id TEXT, connection_id TEXT, payload_json TEXT NOT NULL);
      CREATE INDEX diagnostic_events_lease ON diagnostic_events(lease_id,seq);
      CREATE INDEX diagnostic_events_connection ON diagnostic_events(connection_id,seq);`);
    const journal = new DiagnosticJournal(ctx.storage, "managed");
    journal.record({ type: "hand.lease", thread_id: "thread-a", lease_id: "lease-1" });
    journal.record({ type: "hand.socket", connection_id: "socket-1" });
    journal.record({ type: "hand.socket", lease_id: "lease-1" });
    journal.record({ type: "managed.agent.tool", thread_id: "thread-a", tool: "Bash" });
    journal.record({ type: "managed.agent.tool", thread_id: "thread-a", connection_id: "socket-1" });
    const indexes = sql.exec<{ name: string; sql: string }>("SELECT name, sql FROM sqlite_master WHERE type='index' AND tbl_name='diagnostic_events' ORDER BY name").toArray();
    expect(indexes.map(index => index.name)).toEqual(["diagnostic_events_connection_present", "diagnostic_events_lease_present", "diagnostic_events_thread"]);
    expect(indexes.filter(index => index.name.endsWith("_present")).every(index => /WHERE \w+ IS NOT NULL/.test(index.sql))).toBe(true);
    const page = journal.page("thread-a", 0, 100, true);
    expect(page.events.map(event => event.type)).toEqual(["hand.lease", "hand.socket", "hand.socket", "managed.agent.tool", "managed.agent.tool"]);
    expect(journal.page("thread-a", 0, 100).events).toHaveLength(3);
    const plan = sql.exec<{ detail: string }>(`EXPLAIN QUERY PLAN SELECT seq FROM diagnostic_events WHERE lease_id IN (SELECT DISTINCT lease_id FROM diagnostic_events WHERE thread_id=? AND lease_id IS NOT NULL)`, "thread-a").toArray();
    expect(plan.map(row => row.detail).join("\n")).toContain("diagnostic_events_lease_present");
  });
});
