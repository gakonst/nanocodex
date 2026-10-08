import type { HostedMachine } from "nanocodex-tools/hosted";
import type { AccountHostedTools } from "./account-hosted-tools";

export type HandEnv = {
  NANOCODEX_ACCOUNT_TOOLS?: DurableObjectNamespace<AccountHostedTools>;
};
export const HAND_MACHINE_HEADER = "x-nanocodex-hand-machine-id";
export const HAND_RUNTIME_HEADER = "x-nanocodex-hand-runtime-id";

export function publisherIdentity(headers: Headers): { machineId: string; runtimeId: string } | undefined | false {
  const machineId = headers.get(HAND_MACHINE_HEADER), runtimeId = headers.get(HAND_RUNTIME_HEADER);
  if (machineId === null && runtimeId === null) return undefined;
  return validPublisherId(machineId) && validPublisherId(runtimeId) ? { machineId, runtimeId } : false;
}
export function validPublisherId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}

export type HandPublication = Readonly<{
  route_id: string;
  publication_id: string;
  /** Always "legacy": the owner object. Retained for stored rows. */
  region: "legacy";
  machine: HostedMachine;
  tool_names: readonly string[];
  runtime_id?: string;
}>;
type DirectoryEntry = HandPublication & { pending: boolean; previous: readonly HandPublication[] };

/**
 * Durable runtime ownership of each account Hand. A newer runtime of a machine
 * supersedes the previous one; superseded and forgotten runtimes are retired
 * tombstones that can never publish again.
 */
export class HandDirectory {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_hand_directory (
      machine_id TEXT PRIMARY KEY, publication_json TEXT NOT NULL
    ); CREATE TABLE IF NOT EXISTS regional_hand_placements (
      machine_id TEXT NOT NULL, runtime_id TEXT NOT NULL, region TEXT NOT NULL,
      retired INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(machine_id,runtime_id)
    )`);
    // Publications and live placements of the removed regional relays are dead:
    // forget them so their runtimes republish here. Retired tombstones stay.
    storage.sql.exec("DELETE FROM regional_hand_placements WHERE region<>'legacy' AND retired=0");
    storage.sql.exec("DELETE FROM regional_hand_directory WHERE json_extract(publication_json,'$.region')<>'legacy'");
  }
  entries(): readonly DirectoryEntry[] {
    return this.storage.sql.exec<{ publication_json: string }>("SELECT publication_json FROM regional_hand_directory ORDER BY machine_id")
      .toArray().map(row => JSON.parse(row.publication_json) as DirectoryEntry);
  }
  #known(machineId: string, runtimeId: string): boolean {
    return this.storage.sql.exec("SELECT 1 FROM regional_hand_placements WHERE machine_id=? AND runtime_id=?", machineId, runtimeId).toArray().length > 0;
  }
  retired(machineId: string, runtimeId: string): boolean {
    return this.storage.sql.exec<{ retired: number }>("SELECT retired FROM regional_hand_placements WHERE machine_id=? AND runtime_id=?", machineId, runtimeId).toArray()[0]?.retired === 1;
  }
  #record(machineId: string, runtimeId: string): void {
    if (this.retired(machineId, runtimeId)) throw new Error("Hand runtime was superseded");
    if (!this.#known(machineId, runtimeId)) {
      this.storage.sql.exec("INSERT INTO regional_hand_placements(machine_id,runtime_id,region) VALUES(?,?,'legacy')", machineId, runtimeId);
    }
  }
  retire(machineId: string, runtimeId: string): void {
    if (!this.#known(machineId, runtimeId)) this.#record(machineId, runtimeId);
    this.storage.sql.exec("UPDATE regional_hand_placements SET retired=1 WHERE machine_id=? AND runtime_id=?", machineId, runtimeId);
  }
  /**
   * Owner-initiated removal: retire every runtime of the machine so a late
   * runtime cannot reclaim the identity after the owner forgot it.
   */
  forget(machineId: string): void {
    for (const row of this.storage.sql.exec<{ runtime_id: string }>(
      "SELECT runtime_id FROM regional_hand_placements WHERE machine_id=?", machineId).toArray()) {
      this.retire(machineId, row.runtime_id);
    }
    this.storage.sql.exec("DELETE FROM regional_hand_directory WHERE machine_id=?", machineId);
  }
  #save(entry: DirectoryEntry): void {
    this.storage.sql.exec("INSERT INTO regional_hand_directory VALUES(?,?) ON CONFLICT(machine_id) DO UPDATE SET publication_json=excluded.publication_json", entry.machine.id, JSON.stringify(entry));
  }
  /** Admit `candidate` as the machine's current publication; a newer runtime retires the previous one. */
  claim(candidate: HandPublication): void {
    if (candidate.runtime_id) this.#record(candidate.machine.id, candidate.runtime_id);
    const entries = this.entries();
    const old = entries.find(entry => entry.machine.id === candidate.machine.id);
    if (entries.some(entry => entry.machine.id !== candidate.machine.id && entry.tool_names.some(name => candidate.tool_names.includes(name)))) {
      throw new Error("tool name is already exposed by another account Hand");
    }
    if (old?.runtime_id && candidate.runtime_id && old.runtime_id !== candidate.runtime_id) {
      this.storage.sql.exec("UPDATE regional_hand_placements SET retired=1 WHERE machine_id=? AND runtime_id=?", old.machine.id, old.runtime_id);
    }
    this.#save({ ...candidate, pending: false, previous: [] });
  }
}
