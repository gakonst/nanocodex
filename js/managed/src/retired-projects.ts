/** Remove retired grouping metadata. Conversations, titles and history are untouched. */
export function retireAccountProjects(storage: DurableObjectStorage): void {
  storage.transactionSync(() => {
    storage.sql.exec("DROP TABLE IF EXISTS conversation_projects");
    storage.sql.exec("DROP TABLE IF EXISTS project_threads");
    // Keep the original migration receipt/backup for audit and recovery.
  });
}

export const retiredProjectTools = new Set([
  "spawn_project_thread", "send_project_thread", "read_project_thread", "list_project_threads",
]);

/** Stop the old outbox and cancel only completion turns backed by its durable ledger. */
export function retireSessionProjects(storage: DurableObjectStorage, cancel: (id: string) => void): void {
  const hasRuns = storage.sql.exec("SELECT name FROM sqlite_master WHERE type='table' AND name='project_thread_runs'").toArray().length > 0;
  storage.sql.exec("CREATE TABLE IF NOT EXISTS retired_project_completions (id TEXT PRIMARY KEY)");
  if (hasRuns) {
    storage.sql.exec("INSERT OR IGNORE INTO retired_project_completions SELECT 'project-result:' || id FROM project_thread_runs");
    const queued = storage.sql.exec<{ id: string }>(`SELECT t.id FROM managed_turns t
      JOIN project_thread_runs r ON t.id = 'project-result:' || r.id
      WHERE t.state IN ('accepted','cancelling')`).toArray();
    // Persist cancellation through the normal lifecycle before removing the ledger.
    // On interruption both cancellation and retirement can safely run again.
    for (const row of queued) cancel(row.id);
  }
  storage.transactionSync(() => {
    storage.sql.exec("DROP TABLE IF EXISTS project_thread_runs");
    storage.sql.exec("DROP TABLE IF EXISTS project_spawn_plans");
    const row = storage.sql.exec<{ body: string }>("SELECT body FROM managed_configuration WHERE singleton=1").toArray()[0];
    if (!row) return;
    const configuration = JSON.parse(row.body);
    if (!Array.isArray(configuration.tools)) return;
    const retained = configuration.tools.filter((name: string) => !retiredProjectTools.has(name));
    if (retained.length === configuration.tools.length) return;
    // Keep an explicit empty allowlist empty; retirement must never add authority.
    configuration.tools = retained;
    storage.sql.exec("UPDATE managed_configuration SET body=? WHERE singleton=1", JSON.stringify(configuration));
  });
}

/** A terminal outer receipt can still be reconciled against a pending runtime checkpoint. */
export function isRetiredProjectCompletion(storage: DurableObjectStorage, id: string): boolean {
  return storage.sql.exec("SELECT name FROM sqlite_master WHERE type='table' AND name='retired_project_completions'").toArray().length > 0
    && storage.sql.exec("SELECT id FROM retired_project_completions WHERE id=?", id).toArray().length > 0;
}
