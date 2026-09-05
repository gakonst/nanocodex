export const hostPrincipalExchangeResourcePrefix = "urn:nanocodex:host-principal:exchange:";

const opaqueToken = /^[A-Za-z0-9_-]{43}$/;

export type HostPrincipal = Readonly<{
  kind: "host";
  id: string;
  app_id: string;
  app_origin: string;
  issuer: string;
  tenant: string;
  session_epoch: number;
  session_digest: string;
}>;

export function hostPrincipalExchangeFromResources(resources: unknown): Readonly<{
  exchange: string;
  resources: readonly string[];
}> | undefined {
  if (!Array.isArray(resources)) throw new Error("Host principal resources are invalid.");
  const candidates = resources.filter((resource) => (
    typeof resource === "string" && resource.startsWith(hostPrincipalExchangeResourcePrefix)
  ));
  if (candidates.length === 0) return undefined;
  if (candidates.length !== 1) throw new Error("The host principal exchange resource is invalid.");
  const exchange = candidates[0].slice(hostPrincipalExchangeResourcePrefix.length);
  if (!opaqueToken.test(exchange)) throw new Error("The host principal exchange resource is invalid.");
  return Object.freeze({
    exchange,
    resources: Object.freeze(resources.filter((resource): resource is string => resource !== candidates[0])),
  });
}

export function isHostPrincipal(value: unknown): value is HostPrincipal {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && Object.keys(value).length === 8
    && "kind" in value
    && value.kind === "host"
    && "id" in value
    && typeof value.id === "string"
    && opaqueToken.test(value.id)
    && "app_id" in value
    && typeof value.app_id === "string"
    && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value.app_id)
    && "app_origin" in value
    && isExactOrigin(value.app_origin)
    && "issuer" in value
    && isBoundedClaim(value.issuer)
    && "tenant" in value
    && isBoundedClaim(value.tenant)
    && "session_epoch" in value
    && Number.isSafeInteger(value.session_epoch)
    && Number(value.session_epoch) >= 1
    && "session_digest" in value
    && typeof value.session_digest === "string"
    && opaqueToken.test(value.session_digest);
}

export function sameHostPrincipal(left: unknown, right: unknown): boolean {
  return isHostPrincipal(left)
    && isHostPrincipal(right)
    && left.kind === right.kind
    && left.id === right.id
    && left.app_id === right.app_id
    && left.app_origin === right.app_origin
    && left.issuer === right.issuer
    && left.tenant === right.tenant
    && left.session_epoch === right.session_epoch
    && left.session_digest === right.session_digest;
}

export function sameOrderedResources(left: unknown, right: unknown): boolean {
  return Array.isArray(left)
    && Array.isArray(right)
    && left.length === right.length
    && new Set(left).size === left.length
    && new Set(right).size === right.length
    && left.every((resource, index) => resource === right[index]);
}

function isExactOrigin(value: unknown): boolean {
  if (typeof value !== "string") return false;
  try {
    const url = new URL(value);
    const loopback = url.hostname === "localhost"
      || url.hostname === "127.0.0.1"
      || url.hostname === "[::1]"
      || url.hostname.endsWith(".localhost");
    return url.origin === value
      && !url.username
      && !url.password
      && (url.protocol === "https:" || (loopback && url.protocol === "http:"));
  } catch {
    return false;
  }
}

function isBoundedClaim(value: unknown): boolean {
  return typeof value === "string"
    && /^[^\u0000-\u001f\u007f]{1,512}$/.test(value);
}
