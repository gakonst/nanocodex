/** Missing rows are legacy/unknown. Only fresh sessions can prove their pool empty. */
export function initializeVmHostScopeSchema(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_vm_host_scope (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    may_exist INTEGER NOT NULL CHECK (may_exist IN (0, 1))
  )`);
}

/** Call in the transaction that first creates the session; never reset retained sessions. */
export function initializeEmptyVmHostScope(storage: DurableObjectStorage): void {
  storage.sql.exec("INSERT OR IGNORE INTO managed_vm_host_scope VALUES (1, 0)");
}

/** Fence registration before allowing its upgrade; a failed upgrade stays conservative. */
export function markVmHostScopeRegistration(storage: DurableObjectStorage): void {
  storage.sql.exec(`INSERT INTO managed_vm_host_scope VALUES (1, 1)
    ON CONFLICT(singleton) DO UPDATE SET may_exist = 1`);
}

export function shouldProbeAgentVmHostScope(
  storage: DurableObjectStorage,
  retainedSelection: string | undefined,
): boolean {
  // A retained selection is a durable acquire/release intent. Never skip it.
  if (retainedSelection !== undefined) return true;
  return storage.sql.exec<{ may_exist: number }>(
    "SELECT may_exist FROM managed_vm_host_scope WHERE singleton = 1",
  ).toArray()[0]?.may_exist !== 0;
}
