export type AccountCommunication = Readonly<{ email: string | null; phone: string | null }>;

export function decodeAccountCommunication(value: unknown): AccountCommunication {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid contact details");
  const { email, phone } = value as Record<string, unknown>;
  if (!(email === null || (typeof email === "string" && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)))
    || !(phone === null || (typeof phone === "string" && /^\+[1-9]\d{1,14}$/.test(phone)))) throw new Error("Invalid contact details");
  return { email, phone };
}
