import { DurableMemoryError, parseMemoryOperation, type MemoryOperation } from "./durable-memory";

export type MemoryVisibility = "team" | "personal";
export function memoryVisibility(value: unknown): MemoryVisibility {
  if (value === undefined || value === "team") return "team";
  if (value === "personal") return "personal";
  throw new DurableMemoryError("invalid_request", "memory scope must be team or personal");
}
export function scopedMemoryOperation(value: unknown): { operation: MemoryOperation; scope: MemoryVisibility } {
  if (!value || typeof value !== "object" || Array.isArray(value)) return { operation: parseMemoryOperation(value), scope: "team" };
  const { scope, ...operation } = value as Record<string, unknown>;
  return { operation: parseMemoryOperation(operation), scope: memoryVisibility(scope) };
}
export const personalMemoryTeam = (userId: string): string => `personal:${userId}`;
export function memoryTarget(organizationId: string, teamId: string, userId: string, scope: MemoryVisibility) {
  return scope === "personal"
    ? { name: JSON.stringify(["personal-memory", organizationId, userId]), team: personalMemoryTeam(userId) }
    : { name: organizationId, team: teamId };
}
