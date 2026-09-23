export type MemoryVisibility = "team" | "personal";
export const personalMemoryTeam = (userId: string): string => `personal:${userId}`;
export function memoryTarget(organizationId: string, teamId: string, userId: string, scope: MemoryVisibility) {
  return scope === "personal"
    ? { name: JSON.stringify(["personal-memory", organizationId, userId]), team: personalMemoryTeam(userId) }
    : { name: organizationId, team: teamId };
}
