export type AdminDetails = {
  email: { assigned: boolean; configured: boolean; available: boolean; address: string | null };
  phone: { assigned: boolean; configured: boolean; address: string | null };
};
export function decodeAdminDetails(value: unknown): AdminDetails {
  if (!value || typeof value !== "object") throw new Error("Invalid admin response");
  const { email, phone } = value as AdminDetails;
  for (const service of [email, phone]) {
    if (!service || typeof service.assigned !== "boolean" || typeof service.configured !== "boolean"
      || !(service.address === null || typeof service.address === "string")) throw new Error("Invalid admin response");
  }
  if (typeof email.available !== "boolean") throw new Error("Invalid admin response");
  return { email, phone };
}
