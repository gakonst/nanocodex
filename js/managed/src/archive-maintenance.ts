/** One durable retry deadline for background archival, independent of turn recovery. */
export class ArchiveMaintenance {
  #task?: Promise<void>;

  constructor(
    readonly storage: DurableObjectStorage,
    readonly now: () => number = Date.now,
  ) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_archive_maintenance (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), retry_at INTEGER NOT NULL
    )`);
  }

  nextAttemptAt(): number {
    return this.storage.sql.exec<{ retry_at: number }>(
      "SELECT retry_at FROM managed_archive_maintenance WHERE singleton = 1",
    ).toArray()[0]?.retry_at ?? 0;
  }

  start(work: () => Promise<void>): Promise<void> | undefined {
    if (this.#task || this.nextAttemptAt() > this.now()) return undefined;
    // Publish the recovery deadline before external I/O. A reset or upload
    // failure must not turn a pending archive into a one-millisecond alarm loop.
    this.storage.sql.exec(
      "INSERT OR REPLACE INTO managed_archive_maintenance VALUES (1, ?)",
      this.now() + 60_000,
    );
    const task = Promise.resolve().then(work).then(() => {
      this.storage.sql.exec("DELETE FROM managed_archive_maintenance WHERE singleton = 1");
    }).finally(() => { this.#task = undefined; });
    this.#task = task;
    return task;
  }
}
