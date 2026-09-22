import { namespaceMountRoot } from "nanocodex-tools";

type Machine = Readonly<{ id: string; name: string }>;

/** Labels choose a path once; durable machine identities continue to own it. */
export class HandPaths {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_hand_paths (
      machine_id TEXT PRIMARY KEY, root TEXT NOT NULL UNIQUE
    )`);
  }

  roots(): readonly string[] {
    return this.storage.sql.exec<{ root: string }>("SELECT root FROM managed_hand_paths").toArray().map(row => row.root);
  }

  assign(machines: readonly Machine[], reserved: readonly string[] = []): ReadonlyMap<string, string> {
    const rows = this.storage.sql.exec<{ machine_id: string; root: string }>("SELECT machine_id, root FROM managed_hand_paths").toArray();
    const paths = new Map(rows.map(row => [row.machine_id, row.root]));
    const used = new Set([...reserved, ...paths.values(), "/brain"]);
    const legacy = new Map(machines.map(machine => [namespaceMountRoot(machine.id), machine.id]));
    for (const machine of [...machines].sort((a, b) => a.id.localeCompare(b.id))) {
      if (paths.has(machine.id)) continue;
      const stem = readableHandRoot(machine.name);
      let root = stem;
      let suffix = 2;
      while (used.has(root) || (legacy.has(root) && legacy.get(root) !== machine.id)) {
        const tail = `-${suffix++}`;
        root = `${stem.slice(0, 64 - tail.length).replace(/[._-]+$/, "")}${tail}`;
      }
      this.storage.sql.exec("INSERT INTO managed_hand_paths(machine_id, root) VALUES (?, ?)", machine.id, root);
      paths.set(machine.id, root);
      used.add(root);
    }
    return paths;
  }
}

export function readableHandRoot(name: string): string {
  const slug = name.normalize("NFKD").toLowerCase().replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9._-]+/g, "-").replace(/^[^a-z0-9]+|[^a-z0-9]+$/g, "")
    .slice(0, 58).replace(/[._-]+$/, "") || "hand";
  // Existing namespace validation owns reserved and Windows device names.
  return namespaceMountRoot(slug) === `/${slug}` ? `/${slug}` : `/hand-${slug}`;
}
