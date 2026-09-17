import type { ToolActivity } from "nanocodex-react/agent";
import { decodeVaultEntries, type VaultEntryKind } from "./vaultEntries.ts";

export type VaultIntake = Readonly<{ operation: "create" | "authorize_origin"; vault_id?: string; kind: VaultEntryKind; name?: string; origin?: string }>;
export function decodeVaultIntake(tool: ToolActivity): VaultIntake | undefined {
  if (tool.name.split(".").at(-1) !== "request_vault_intake" || tool.status !== "completed" || !tool.output) return;
  let value: unknown;
  try { value = JSON.parse(tool.output); } catch { return; }
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const record = value as Record<string, unknown>;
  if (record.type !== "vault_intake" || record.status !== "input_required"
    || !["login", "api_key", "card", "address", "phone"].includes(String(record.kind))
    || Object.keys(record).some(key => !["type", "status", "operation", "vault_id", "kind", "name", "origin"].includes(key))
    || (record.name !== undefined && (typeof record.name !== "string" || !record.name.trim() || record.name.length > 120 || /[\u0000-\u001f\u007f]/.test(record.name)))) return;
  const operation = record.operation ?? "create";
  if (operation !== "create" && operation !== "authorize_origin") return;
  if (operation === "create" && record.vault_id !== undefined) return;
  if (operation === "authorize_origin" && (record.kind !== "login" || typeof record.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(record.vault_id) || record.origin === undefined)) return;
  if (record.origin !== undefined) {
    if (record.kind !== "login" || typeof record.origin !== "string" || record.origin.length > 2048) return;
    try { const url = new URL(record.origin); if (url.protocol !== "https:" || url.origin !== record.origin) return; } catch { return; }
  }
  return { operation, ...(typeof record.vault_id === "string" ? { vault_id: record.vault_id } : {}), kind: record.kind as VaultEntryKind, ...(typeof record.name === "string" ? { name: record.name } : {}), ...(typeof record.origin === "string" ? { origin: record.origin } : {}) };
}

/** Never forward arbitrary Vault response properties into the model transcript. */
export function vaultIntakeReceipt(value: unknown, intake: VaultIntake): string {
  const entry = decodeVaultEntries([value])[0]!;
  const origin = (value as Record<string, unknown>).browser_origin;
  if (entry.kind !== intake.kind || (intake.vault_id !== undefined && entry.id !== intake.vault_id)
    || (intake.origin !== undefined && origin !== intake.origin)) throw new Error("Invalid Vault receipt");
  if (origin !== undefined) {
    if (entry.kind !== "login" || typeof origin !== "string") throw new Error("Invalid Vault receipt");
    const url = new URL(origin);
    if (url.protocol !== "https:" || url.origin !== origin) throw new Error("Invalid Vault receipt");
  }
  return JSON.stringify({ type: "vault_intake_receipt", operation: intake.operation, status: "saved", id: entry.id, kind: entry.kind, name: entry.name, ...(origin === undefined ? {} : { browser_origin: origin }) });
}
