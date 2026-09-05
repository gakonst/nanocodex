import { Kv } from "accounts/server";

import {
  ensureAccount,
  resolveChiefOfStaffPrincipal,
  type AccountAuthEnv,
  type Principal,
} from "./account-auth";

const SLACK_TEAM_ID = /^T[A-Z0-9]+$/;
const SLACK_USER_ID = /^[UW][A-Z0-9]+$/;
const WHATSAPP_PHONE_NUMBER_ID = /^[0-9]{5,32}$/;
const WHATSAPP_USER_ID = /^(?:[0-9]{5,32}|[A-Z]{2}\.(?:ENT\.)?[A-Za-z0-9]{1,128})$/;
const VIBER_BOT_URI = /^[A-Za-z0-9_.-]{1,255}$/;
const VIBER_USER_ID = /^[A-Za-z0-9+/=_-]{1,256}$/;

export type ChiefOfStaffIdentity = Readonly<{
  provider: "slack" | "viber" | "whatsapp";
  subject: string;
  tenant: string;
}>;

export interface ChiefOfStaffPrincipalEnv extends AccountAuthEnv {
  NANOCODEX_CHIEF_EGRESS: Readonly<{
    ensureCredential(userId: unknown): Promise<void>;
  }>;
}

type IdentityMapping = ChiefOfStaffIdentity & Readonly<{
  digest: string;
  userId: string;
}>;

export async function resolveChiefOfStaffIdentity(
  env: ChiefOfStaffPrincipalEnv,
  value: unknown,
): Promise<Principal> {
  const identity = chiefOfStaffIdentity(value);
  if (!identity) throw new Error("invalid_chief_identity");
  const identityDigest = await digest(
    `chief-of-staff:v1\0${identity.provider}\0${identity.tenant}\0${identity.subject}`,
  );
  const key = `identity:${identityDigest}`;
  const store = Kv.durableObject(
    env.NANOCODEX_AUTH as unknown as Parameters<typeof Kv.durableObject>[0],
    { name: "chief-of-staff-identities" },
  );
  let mapping = await store.get<unknown>(key);
  if (mapping === undefined) {
    const candidate = {
      ...identity,
      digest: identityDigest,
      userId: crypto.randomUUID(),
    } satisfies IdentityMapping;
    mapping = await store.create?.(key, candidate) ? candidate : await store.get<unknown>(key);
  }
  if (!validMapping(mapping, identity, identityDigest)) {
    throw new Error("chief_identity_conflict");
  }
  await ensureAccount(env, mapping.userId, true);
  await env.NANOCODEX_CHIEF_EGRESS.ensureCredential(mapping.userId);
  const principal = await resolveChiefOfStaffPrincipal(
    env,
    mapping.userId,
    `chief:${identityDigest}`,
  );
  if (!principal) throw new Error("chief_identity_unavailable");
  return principal;
}

export function chiefOfStaffIdentity(value: unknown): ChiefOfStaffIdentity | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const record = value as Record<string, unknown>;
  if (Object.keys(record).length !== 3
    || typeof record.provider !== "string"
    || typeof record.tenant !== "string"
    || typeof record.subject !== "string") return undefined;
  switch (record.provider) {
    case "slack":
      return SLACK_TEAM_ID.test(record.tenant) && SLACK_USER_ID.test(record.subject)
        ? { provider: "slack", tenant: record.tenant, subject: record.subject }
        : undefined;
    case "viber":
      return VIBER_BOT_URI.test(record.tenant) && VIBER_USER_ID.test(record.subject)
        ? { provider: "viber", tenant: record.tenant, subject: record.subject }
        : undefined;
    case "whatsapp":
      return WHATSAPP_PHONE_NUMBER_ID.test(record.tenant) && WHATSAPP_USER_ID.test(record.subject)
        ? { provider: "whatsapp", tenant: record.tenant, subject: record.subject }
        : undefined;
    default: return undefined;
  }
}

function validMapping(
  value: unknown,
  identity: ChiefOfStaffIdentity,
  identityDigest: string,
): value is IdentityMapping {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  return Object.keys(record).length === 5
    && record.provider === identity.provider
    && record.tenant === identity.tenant
    && record.subject === identity.subject
    && record.digest === identityDigest
    && typeof record.userId === "string"
    && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(record.userId);
}

async function digest(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
