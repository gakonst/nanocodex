const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
const MAX_VAULT_ENTRIES = 100;
const encoder = new TextEncoder();

/** Rebuilds the metadata-only Vault view exposed by Connect account-info. */
export function projectVaultEntries(value) {
  if (!Array.isArray(value) || value.length > MAX_VAULT_ENTRIES) {
    throw new TypeError("invalid Vault metadata list");
  }
  const seen = new Set();
  return value.map((entry) => {
    if (!isRecord(entry) || !VAULT_ID.test(entry.id) || seen.has(entry.id)) {
      throw new TypeError("invalid Vault metadata entry");
    }
    seen.add(entry.id);
    const common = {
      id: entry.id,
      kind: vaultKind(entry.kind),
      name: text(entry.name, 120),
      created_at: timestamp(entry.created_at),
    };
    switch (common.kind) {
      case "api_key": return common;
      case "login": return { ...common, username: text(entry.username, 512) };
      case "card": return { ...common, last4: last4(entry.last4) };
      case "address": return {
        ...common,
        address_line_1: text(entry.address_line_1, 256),
        ...(entry.address_line_2 === undefined
          ? {}
          : { address_line_2: text(entry.address_line_2, 256) }),
        city: text(entry.city, 120),
        state: text(entry.state, 120),
        zip: text(entry.zip, 32),
        country: text(entry.country, 120),
      };
      case "phone": return { ...common, phone_number: text(entry.phone_number, 64) };
    }
  });
}

function vaultKind(value) {
  if (value !== "login" && value !== "api_key" && value !== "card" && value !== "address" && value !== "phone") {
    throw new TypeError("invalid Vault metadata kind");
  }
  return value;
}

function text(value, maxBytes) {
  if (typeof value !== "string" || value.length === 0 || encoder.encode(value).byteLength > maxBytes) {
    throw new TypeError("invalid Vault metadata text");
  }
  return value;
}

function timestamp(value) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError("invalid Vault metadata timestamp");
  }
  return value;
}

function last4(value) {
  if (typeof value !== "string" || !/^\d{4}$/.test(value)) {
    throw new TypeError("invalid Vault card metadata");
  }
  return value;
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
