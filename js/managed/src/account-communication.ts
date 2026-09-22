import { phoneAdminConfigured, type PhoneAdminConfig } from "./phone-admin";
import type { EmailConfig } from "./email-tool";

type CommunicationConfig = EmailConfig & PhoneAdminConfig & {
  TWILIO_VOICE_FROM_NUMBER?: string;
};

/** Assignments are operator-scoped, never inferred from login or Vault identity. */
export async function accountCommunication(config: CommunicationConfig, owner: string): Promise<{
  email: string | null; phone: string | null;
}> {
  let email: string | null = null;
  const phone = owner && phoneAdminConfigured(config) && owner === config.NANOCODEX_PHONE_OWNER_ID
    && /^\+[1-9]\d{1,14}$/.test(config.TWILIO_VOICE_FROM_NUMBER ?? "")
    ? config.TWILIO_VOICE_FROM_NUMBER! : null;
  if (owner && owner === config.NANOCODEX_EMAIL_ADMIN_ID
    && owner === config.NANOCODEX_EMAIL_OWNER_ID && config.NANOCODEX_EMAIL) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const status = await Promise.race([
        config.NANOCODEX_EMAIL.execute({ operation: "status", owner_id: owner, agent_id: "account-communication" }),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("Email status unavailable")), 5_000);
        }),
      ]);
      if (!status || typeof status !== "object" || Array.isArray(status)) throw new Error("Invalid email status");
      const value = status as Record<string, unknown>;
      if (value.configured !== true || typeof value.address !== "string"
        || value.address.length > 254 || !/^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(value.address)) {
        throw new Error("Invalid email status");
      }
      email = value.address;
    } finally { clearTimeout(timer); }
  }
  return { email, phone };
}
