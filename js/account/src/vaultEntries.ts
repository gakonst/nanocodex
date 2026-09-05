export type VaultEntryKind = "login" | "card" | "address" | "phone";

export type VaultEntryMetadata = Readonly<{
  id: string;
  kind: VaultEntryKind;
  name: string;
  createdAt: number;
}>;

const ENTRY_ID = /^[A-Za-z0-9_-]{22,64}$/;
const ENTRY_KINDS = new Set<VaultEntryKind>(["login", "card", "address", "phone"]);

export function decodeVaultEntries(value: unknown): readonly VaultEntryMetadata[] {
  if (!Array.isArray(value)) throw new Error("Invalid vault response.");
  return value.map((candidate) => {
    if (!isRecord(candidate)) throw new Error("Invalid vault response.");
    const { id, kind, name, created_at: createdAt } = candidate;
    if (typeof id !== "string" || !ENTRY_ID.test(id)
      || typeof kind !== "string" || !ENTRY_KINDS.has(kind as VaultEntryKind)
      || typeof name !== "string" || !name.trim() || name.length > 120
      || typeof createdAt !== "number" || !Number.isFinite(createdAt)) {
      throw new Error("Invalid vault response.");
    }
    return { id, kind: kind as VaultEntryKind, name, createdAt };
  });
}

export function vaultEntryPath(kind: VaultEntryKind, id?: string): string {
  const base = `/v1/credentials/vault/${kind}`;
  if (id === undefined) return base;
  if (!ENTRY_ID.test(id)) throw new Error("Invalid vault entry ID.");
  return `${base}/${encodeURIComponent(id)}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
