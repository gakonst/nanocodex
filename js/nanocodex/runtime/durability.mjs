const hosts = new Map();
let nextRouteId = 1n;

export function own(host, store, stateId) {
  if ((store === undefined) !== (stateId === undefined)) {
    throw new TypeError("durability and durabilityId must be supplied together");
  }
  const routeId = `durability-route-${nextRouteId++}`;
  hosts.set(routeId, { host, store, stateId, references: 0 });
  return Object.freeze({
    id: routeId,
    abandon: () => abandon(host, routeId),
    retain: () => retain(host, routeId),
    release: () => release(host, routeId),
  });
}

export function retain(host, routeId) {
  const ownership = hosts.get(routeId);
  if (!ownership || ownership.host !== host) {
    throw new Error(`Nanocodex durability route is not bound to this host: ${routeId}`);
  }
  ownership.references += 1;
}

export function release(host, routeId) {
  const ownership = hosts.get(routeId);
  if (!ownership || ownership.host !== host) return;
  if (ownership.references > 0) ownership.references -= 1;
  if (ownership.references === 0) hosts.delete(routeId);
}

export function abandon(host, routeId) {
  const ownership = hosts.get(routeId);
  if (ownership?.host === host && ownership.references === 0) hosts.delete(routeId);
}

export async function acquire(routeId, stateId, ownerId) {
  const stored = await requiredRoute(routeId).store.acquire(stateId, { ownerId });
  if (!stored || typeof stored !== "object" || Array.isArray(stored)) {
    throw new TypeError("durability.acquire() must return an acquired state object");
  }
  exactKeys(stored, ["ownerId", "fence", "revision", "payload"], "durability acquired state");
  const acquiredOwnerId = nonempty(stored.ownerId, "durability owner ID");
  if (acquiredOwnerId !== ownerId) {
    throw new TypeError("durability.acquire() must return the requested owner ID");
  }
  const retainedRevision = uint64(stored.revision, "durability state revision");
  const payload = stored.payload === null
    ? null
    : string(stored.payload, "durability state payload");
  if ((retainedRevision === "0") !== (payload === null)) {
    throw new TypeError("durability.acquire() returned inconsistent revision and payload");
  }
  // Cross the WASM boundary as fields. Wrapping a large legacy payload in
  // another JSON string duplicates it in both JS and WASM during cold recovery.
  return Object.freeze({
    owner_id: acquiredOwnerId,
    fence: uint64(stored.fence, "durability owner fence"),
    revision: retainedRevision,
    // wasm-bindgen's string conversion can reserve three times a large
    // Unicode string's length. UTF-8 bytes cross with one exact allocation.
    payload: payload === null ? null : new TextEncoder().encode(payload),
  });
}

export async function replace(
  routeId,
  stateId,
  ownerId,
  fence,
  expectedRevision,
  payload,
) {
  const result = await requiredRoute(routeId).store.replace(stateId, {
    ownerId,
    fence,
    expectedRevision,
    payload,
  });
  if (result?.status === "replaced") {
    exactKeys(result, ["status", "revision"], "durability replaced result");
    return JSON.stringify({
      status: "replaced",
      revision: uint64(result.revision, "durability replace revision"),
    });
  }
  if (result?.status === "conflict") {
    exactKeys(result, ["status", "actualRevision"], "durability conflict result");
    return JSON.stringify({
      status: "conflict",
      actual_revision: uint64(result.actualRevision, "durability conflict revision"),
    });
  }
  if (result?.status === "not_committed") {
    exactKeys(result, ["status", "message"], "durability not-committed result");
    return JSON.stringify({
      status: "not_committed",
      message: nonempty(result.message, "durability not-committed message"),
    });
  }
  if (result?.status === "fenced") {
    exactKeys(result, ["status"], "durability fenced result");
    return JSON.stringify({ status: "fenced" });
  }
  throw new TypeError(
    "durability.replace() must return a replaced, conflict, fenced, or not_committed result",
  );
}

function requiredRoute(routeId) {
  const route = hosts.get(routeId);
  if (!route) throw new Error(`no Nanocodex host owns durability route: ${routeId}`);
  const store = route.store;
  if (!store || typeof store.acquire !== "function" || typeof store.replace !== "function") {
    throw new TypeError("the selected Nanocodex host must define a durability state store");
  }
  return route;
}

function uint64(value, name) {
  if (
    typeof value !== "string"
    || !/^(0|[1-9][0-9]*)$/.test(value)
    || BigInt(value) > 18_446_744_073_709_551_615n
  ) {
    throw new TypeError(`${name} must be an unsigned 64-bit decimal string`);
  }
  return value;
}

function string(value, name) {
  if (typeof value !== "string") throw new TypeError(`${name} must be a string`);
  return value;
}

function nonempty(value, name) {
  if (typeof value !== "string" || !value.trim()) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  return value;
}

function exactKeys(value, expected, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${name} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const required = [...expected].sort();
  if (actual.length !== required.length || actual.some((key, index) => key !== required[index])) {
    throw new TypeError(`${name} must contain exactly ${required.join(", ")}`);
  }
}
