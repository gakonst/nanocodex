/** Exact, signed standalone authority. Absence never inherits owner/agent access. */
export type ServiceCapabilities = Readonly<{
  vault?: Readonly<{ ids: readonly string[]; origins: readonly string[]; request: boolean }>;
  phone?: Readonly<{ numberIds: readonly string[]; read: boolean; provision: boolean; release: boolean }>;
}>;
export const serviceResourcePrefix = "urn:nanocodex:services:";
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const exact = (v: Record<string, unknown>, keys: readonly string[]) => Object.keys(v).every(k => keys.includes(k));
const ids = (v: unknown): v is string[] => Array.isArray(v) && v.length <= 64 && new Set(v).size === v.length
  && v.every(id => typeof id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(id));
export function isServiceCapabilities(v: unknown): v is ServiceCapabilities {
  if (!record(v) || !exact(v, ["vault", "phone"])) return false;
  if (v.vault !== undefined) {
    const s = v.vault;
    if (!record(s) || !exact(s, ["ids", "origins", "request"]) || !ids(s.ids) || typeof s.request !== "boolean"
      || !Array.isArray(s.origins) || s.origins.length > 64 || new Set(s.origins).size !== s.origins.length
      || !s.origins.every(origin => {
        if (typeof origin !== "string" || origin.length > 2048) return false;
        try { const u = new URL(origin); return u.protocol === "https:" && u.origin === origin && !u.username && !u.password; }
        catch { return false; }
      })) return false;
  }
  if (v.phone !== undefined) {
    const s = v.phone;
    if (!record(s) || !exact(s, ["numberIds", "read", "provision", "release"]) || !ids(s.numberIds)
      || typeof s.read !== "boolean" || typeof s.provision !== "boolean" || typeof s.release !== "boolean") return false;
  }
  return true;
}
export function approvedServices(resources: readonly string[]): ServiceCapabilities | undefined {
  const matches = resources.filter(r => r.startsWith(serviceResourcePrefix));
  if (matches.length === 0) return undefined;
  if (matches.length !== 1 || matches[0]!.length > 12288) throw new Error("Service authority requires exactly one signed resource.");
  const value: unknown = JSON.parse(decodeURIComponent(matches[0]!.slice(serviceResourcePrefix.length)));
  if (!isServiceCapabilities(value)) throw new Error("Invalid signed service capabilities.");
  return value;
}
