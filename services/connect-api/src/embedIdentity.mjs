export const identitySessionResourcePrefix = "urn:nanocodex:identity-session:";

const appIdPattern = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const secretDigestPattern = /^[A-Za-z0-9_-]{43}$/;
const subjectPattern = /^[^\u0000-\u001f\u007f]{1,512}$/;
const tokenPattern = /^[A-Za-z0-9_-]{43}$/;

export function parseEmbedProjects(encoded) {
  if (encoded === undefined || encoded === "") return Object.freeze([]);
  let value;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new Error("NANOCODEX_EMBED_PROJECTS must be valid JSON.");
  }
  if (!Array.isArray(value) || value.length > 128) {
    throw new Error("NANOCODEX_EMBED_PROJECTS must be a bounded array.");
  }
  const identities = new Set();
  return Object.freeze(value.map((entry) => {
    if (!isRecord(entry)
      || Object.keys(entry).some((key) => !["app_id", "app_origin", "secret_sha256"].includes(key))
      || !appIdPattern.test(entry.app_id)
      || !secretDigestPattern.test(entry.secret_sha256)) {
      throw new Error("NANOCODEX_EMBED_PROJECTS contains an invalid project.");
    }
    const appOrigin = publicOrigin(entry.app_origin);
    const identity = `${entry.app_id}\u0000${appOrigin}`;
    if (identities.has(identity)) {
      throw new Error("NANOCODEX_EMBED_PROJECTS contains a duplicate app and origin.");
    }
    identities.add(identity);
    return Object.freeze({
      appId: entry.app_id,
      appOrigin,
      secretSha256: entry.secret_sha256,
    });
  }));
}

export async function authenticateEmbedProject(encoded, parameters) {
  if (!parameters || !appIdPattern.test(parameters.appId)
    || typeof parameters.secret !== "string" || !/^\S{32,512}$/.test(parameters.secret)) return undefined;
  let appOrigin;
  try { appOrigin = publicOrigin(parameters.appOrigin); } catch { return undefined; }
  const project = parseEmbedProjects(encoded).find((candidate) => (
    candidate.appId === parameters.appId && candidate.appOrigin === appOrigin
  ));
  if (!project) return undefined;
  const actual = await sha256Base64Url(parameters.secret);
  return constantTimeEqual(actual, project.secretSha256) ? project : undefined;
}

export function parseEmbedSessionBody(value) {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["app_origin", "expires_in", "organization", "subject"].includes(key))
    || !subjectPattern.test(value.subject)
    || (value.organization !== undefined && !subjectPattern.test(value.organization))) {
    throw new Error("The embedded identity session is invalid.");
  }
  const expiresIn = value.expires_in ?? 300;
  if (!Number.isSafeInteger(expiresIn) || expiresIn < 30 || expiresIn > 300) {
    throw new Error("The embedded identity session expiry is invalid.");
  }
  return Object.freeze({
    appOrigin: publicOrigin(value.app_origin),
    expiresIn,
    subject: value.subject,
    ...(value.organization === undefined ? {} : { organization: value.organization }),
  });
}

export function identitySessionToken(resources) {
  if (!Array.isArray(resources)) throw new Error("Connect resources are invalid.");
  const tokens = resources.flatMap((resource) => (
    typeof resource === "string" && resource.startsWith(identitySessionResourcePrefix)
      ? [resource.slice(identitySessionResourcePrefix.length)]
      : []
  ));
  if (tokens.length === 0) return undefined;
  if (tokens.length !== 1 || !tokenPattern.test(tokens[0])) {
    throw new Error("The embedded identity session resource is invalid.");
  }
  return tokens[0];
}

export async function embedPrincipalId(identity) {
  if (!isEmbedIdentity(identity)) throw new Error("The embedded identity is invalid.");
  return sha256Base64Url(`${identity.appId}\u0000${identity.issuer}\u0000${identity.subject}`);
}

export function isEmbedIdentity(value) {
  return isRecord(value)
    && appIdPattern.test(value.appId)
    && publicOriginOrFalse(value.appOrigin)
    && typeof value.issuer === "string"
    && value.issuer === `urn:nanocodex:app:${value.appId}`
    && subjectPattern.test(value.subject)
    && (value.organization === undefined || subjectPattern.test(value.organization));
}

export async function sha256Base64Url(value) {
  const digest = new Uint8Array(await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  ));
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function constantTimeEqual(left, right) {
  if (typeof left !== "string" || typeof right !== "string" || left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left.charCodeAt(index) ^ right.charCodeAt(index);
  }
  return difference === 0;
}

function publicOrigin(value) {
  if (typeof value !== "string") throw new Error("The embedded app origin is invalid.");
  const url = new URL(value);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
  if (url.origin !== value || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))
    || url.username || url.password) {
    throw new Error("The embedded app origin is invalid.");
  }
  return url.origin;
}

function publicOriginOrFalse(value) {
  try { return publicOrigin(value) === value; } catch { return false; }
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
