/** Present an already-authorized, sanitized account projection to the model.
 * Authorization and secret filtering remain the responsibility of the host. */
export function projectEnvironment(info, { runtime, default_cwd }) {
  const services = new Set([
    ...(info.authenticated ?? []), ...Object.keys(info.connectorAccounts ?? {}),
    ...Object.keys(info.connectorTools ?? {}),
  ]);
  return {
    runtime, default_cwd, status: info.status,
    hands: Object.fromEntries((info.machines ?? []).map((hand) => [hand.id, {
      name: hand.name, path: hand.mount, capabilities: [...hand.capabilities],
      ...(hand.kind === undefined ? {} : { kind: hand.kind }),
      ...(hand.online === undefined ? {} : { online: hand.online }),
      ...(hand.provider === undefined ? {} : { provider: hand.provider }),
      ...(hand.vm_provider === undefined ? {} : { vm_provider: hand.vm_provider }),
    }])),
    accounts: Object.fromEntries([...services].map((service) => [service, {
      connections: (info.connectorAccounts?.[service] ?? []).map(({ id, label, accountId, capabilities }) => ({
        id, label, ...(accountId === undefined ? {} : { accountId }),
        ...(capabilities === undefined ? {} : { capabilities: [...capabilities] }),
      })),
      ...(info.accounts?.[service] === undefined ? {} : { label: info.accounts[service] }),
      ...(info.connectorTools?.[service] === undefined ? {} : { ...info.connectorTools[service] }),
    }])),
    apis: info.apis, identity: info.identity, stablecoins: info.stablecoins,
    authorizations: info.authorizations, vault: info.vault,
  };
}

/** XML delimiters must never be supplied by labels, memories, or other data. */
export function contextData(tag, value) {
  if (!/^[a-z][a-z0-9_]*$/.test(tag)) throw new TypeError("invalid context tag");
  const text = JSON.stringify(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  return `<${tag}>\n${text}\n</${tag}>`;
}

/** Client-reported context is descriptive data, never identity or authorization. */
export function requestOriginContext(value, now = Date.now()) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).some(key => !["client", "hand", "cwd", "timezone", "location"].includes(key))) {
    throw new TypeError("invalid request origin");
  }
  const result = {};
  for (const key of ["client", "hand", "cwd", "timezone"]) {
    const text = value[key];
    if (text === undefined) continue;
    if (typeof text !== "string" || !text.length || text.length > (key === "cwd" ? 512 : 128)
      || !/^[\x20-\x7e]+$/.test(text)) throw new TypeError(`invalid request origin ${key}`);
    result[key] = text;
  }
  if (result.client && !/^[A-Za-z0-9_.-]+$/.test(result.client)) throw new TypeError("invalid request origin client");
  if (result.cwd && (!result.cwd.startsWith("/") || result.cwd.includes("\\")
    || result.cwd.split("/").some(part => part === "." || part === ".."))) throw new TypeError("invalid request origin cwd");
  if (result.timezone) {
    try { new Intl.DateTimeFormat("en-US", { timeZone: result.timezone }); }
    catch { throw new TypeError("invalid request origin timezone"); }
  }
  const location = requestOriginLocation(value.location, now);
  if (location) result.location = location;
  return result;
}

/** Optional sensor data is bounded and revalidated when projected, never inferred from a Hand. */
export function requestOriginLocation(value, now = Date.now()) {
  if (!value || typeof value !== "object" || Array.isArray(value) || !Number.isFinite(now)) return undefined;
  const { latitude, longitude, accuracy_meters, timestamp_ms, approximate } = value;
  if (![latitude, longitude, accuracy_meters, timestamp_ms].every(Number.isFinite)
    || latitude < -90 || latitude > 90 || longitude < -180 || longitude > 180
    || accuracy_meters < 0 || accuracy_meters > 100_000
    || timestamp_ms < now - 300_000 || timestamp_ms > now + 30_000
    || typeof approximate !== "boolean") return undefined;
  return { latitude, longitude, accuracy_meters, timestamp_ms, approximate };
}
