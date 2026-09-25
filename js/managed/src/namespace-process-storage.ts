import type { DurableProcessBinding, NamespaceProcessStorage } from "./namespace-tools";
import type { SandboxOutputCursorStorage } from "./sandbox-tools";

/** DO-local ownership, independent of the ephemeral Agent/tool catalog. */
export class NamespaceProcessSessions implements NamespaceProcessStorage {
  private initialized = false;
  constructor(private readonly storage: DurableObjectStorage) {}

  private ensureSchema(): void {
    if (this.initialized) return;
    this.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_namespace_processes (
        id INTEGER PRIMARY KEY, binding_json TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS managed_sandbox_output_cursors (
        resource_id TEXT NOT NULL, key TEXT NOT NULL, cursor INTEGER NOT NULL,
        PRIMARY KEY (resource_id, key)
      );
    `);
    this.initialized = true;
  }

  get(id: number): DurableProcessBinding | undefined {
    this.ensureSchema();
    const row = this.storage.sql.exec<{ binding_json: string }>(
      "SELECT binding_json FROM managed_namespace_processes WHERE id = ?", id,
    ).toArray()[0];
    return row === undefined ? undefined : JSON.parse(row.binding_json) as DurableProcessBinding;
  }

  put(id: number, binding: DurableProcessBinding): void {
    this.ensureSchema();
    this.storage.sql.exec("INSERT INTO managed_namespace_processes (id, binding_json) VALUES (?, ?)",
      id, JSON.stringify(binding));
  }

  delete(id: number): void {
    this.ensureSchema();
    this.storage.sql.exec("DELETE FROM managed_namespace_processes WHERE id = ?", id);
  }

  outputCursors(resourceId: string): SandboxOutputCursorStorage {
    return {
      get: (key) => {
        this.ensureSchema();
        return this.storage.sql.exec<{ cursor: number }>(
          "SELECT cursor FROM managed_sandbox_output_cursors WHERE resource_id = ? AND key = ?", resourceId, key,
        ).toArray()[0]?.cursor;
      },
      put: (key, cursor) => {
        this.ensureSchema();
        if (!Number.isSafeInteger(cursor) || Number(cursor) < 0) throw new Error("invalid sandbox output cursor");
        this.storage.sql.exec(
          "INSERT OR REPLACE INTO managed_sandbox_output_cursors (resource_id, key, cursor) VALUES (?, ?, ?)", resourceId, key, Number(cursor),
        );
      },
      delete: (key) => {
        this.ensureSchema();
        this.storage.sql.exec("DELETE FROM managed_sandbox_output_cursors WHERE resource_id = ? AND key = ?", resourceId, key);
      },
    };
  }
}
