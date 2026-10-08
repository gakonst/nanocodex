/** Fence this object's screen hosts of a machine (except `keep`) and return the
 * host-sequence high-water mark at the fence. */
export type ScreenFencer = (machineId: string, keep: string | undefined, reason: ScreenFenceReason) => number;
export type ScreenFenceReason = "host_replaced" | "publisher_revoked";
export type ScreenHost = Readonly<{ generation: string }>;
type Row = { machine_id: string; region: string; generation: string; pending_fences: string; watermarks: string };

/**
 * Which host generation may publish a machine's screens. Host sockets are
 * numbered monotonically; a fence records the high-water mark, so a claim from
 * a socket accepted before the fence is rejected forever.
 */
export class ScreenAuthority {
  constructor(private readonly storage: DurableObjectStorage, private readonly fence: ScreenFencer) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_screen_hosts (
      machine_id TEXT PRIMARY KEY, region TEXT NOT NULL, generation TEXT NOT NULL,
      pending_fences TEXT NOT NULL DEFAULT '[]', watermarks TEXT NOT NULL DEFAULT '{}'
    )`);
  }

  #row(machineId: string): Row | undefined {
    return this.storage.sql.exec<Row>("SELECT * FROM regional_screen_hosts WHERE machine_id=?", machineId).toArray()[0];
  }
  #mark(row: Row | undefined): number {
    return row ? (JSON.parse(row.watermarks) as Record<string, number>).legacy ?? 0 : 0;
  }
  #save(machineId: string, generation: string, mark: number): void {
    this.storage.sql.exec(`INSERT INTO regional_screen_hosts(machine_id,region,generation,pending_fences,watermarks) VALUES(?,'legacy',?,'[]',?)
      ON CONFLICT(machine_id) DO UPDATE SET region='legacy',generation=excluded.generation,
      pending_fences='[]',watermarks=excluded.watermarks`, machineId, generation, JSON.stringify({ legacy: mark }));
  }

  /** Grant `generation` (host socket `sequence`). The broker replaces the machine's other hosts. */
  async claim(machineId: string, generation: string, sequence: number): Promise<boolean> {
    const row = this.#row(machineId);
    const mark = this.#mark(row);
    if (!Number.isSafeInteger(sequence) || sequence <= mark) return false;
    if (row?.region === "legacy" && row.generation === generation) return true;
    this.#save(machineId, generation, Math.max(mark, sequence));
    return true;
  }

  /** Withdraw a machine's screen authority and fence its hosts. */
  async revoke(machineId: string): Promise<boolean> {
    const row = this.#row(machineId);
    if (!row) return true;
    this.#save(machineId, "", Math.max(this.#mark(row), this.fence(machineId, undefined, "publisher_revoked")));
    return true;
  }

  /** Authority rows, including withdrawn ones (empty generation lists nothing). */
  hosts(): Map<string, ScreenHost & { region: string }> {
    return new Map(this.storage.sql.exec<Row>("SELECT * FROM regional_screen_hosts").toArray()
      .map(row => [row.machine_id, { region: row.region, generation: row.generation }]));
  }
}

/** Whether a local surface may be listed under the current authority. */
export function screenAuthorized(authority: ReadonlyMap<string, { region: string; generation: string }>, machineId: string, generation: string): boolean {
  const current = authority.get(machineId);
  // Publications that predate authority rows stay visible.
  if (!current) return true;
  return current.region === "legacy" && current.generation === generation;
}
