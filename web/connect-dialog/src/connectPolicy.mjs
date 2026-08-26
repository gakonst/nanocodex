export const productionConnectApiOrigin = "https://nanocodex-connect-api.gakonst.workers.dev";

const appResourcePrefix = "urn:nanocodex:app:";
const appOriginResourcePrefix = "urn:nanocodex:origin:";

const signedAppVisibility = Object.freeze([
  Object.freeze({
    resource: "urn:nanocodex:agent:output:final",
    name: "reply",
    label: "Reply",
    detail: "Final agent reply",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:output:actions",
    name: "actions",
    label: "Actions",
    detail: "Agent actions and tool calls",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:history:read",
    name: "history",
    label: "History",
    detail: "Conversation history",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:trace:read",
    name: "traces",
    label: "Traces",
    detail: "Full run trace",
  }),
]);

const productionApps = new Map([
  ["https://nanocodex-connect-playground.gakonst.workers.dev", Object.freeze({
    id: "atlas-workspace",
    name: "Atlas Workspace",
    origin: "https://nanocodex-connect-playground.gakonst.workers.dev",
  })],
  ["chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle", Object.freeze({
    id: "nanocodex-chrome",
    name: "Nanocodex for Chrome",
    origin: "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle",
  })],
]);

export function registeredApp(embeddingOrigin, appId, dialogUrl, isTopLevel, allowDynamicPopup = true) {
  if (!isAppId(appId)) throw new Error("Nanocodex Connect received an invalid app ID.");
  const dialogOrigin = originFromUrl(dialogUrl, "Nanocodex Connect received an invalid dialog URL.");
  const registered = productionApps.get(embeddingOrigin);
  if (registered) {
    if (registered.id !== appId) throw new Error("This application ID does not match its registered origin.");
    return registered;
  }
  if (isLocalDevelopmentOrigin(dialogOrigin) && isLocalDevelopmentOrigin(embeddingOrigin)) {
    return Object.freeze({ id: appId, name: "Atlas Workspace", origin: embeddingOrigin });
  }
  if (allowDynamicPopup && isPopupPresentation(dialogUrl, isTopLevel) && isSecurePopupOrigin(embeddingOrigin)) {
    const url = new URL(embeddingOrigin);
    return Object.freeze({ id: appId, name: url.hostname, origin: embeddingOrigin });
  }
  throw new Error("This application is not registered with Nanocodex Connect.");
}

export function isPopupPresentation(dialogUrl, isTopLevel) {
  try {
    const url = new URL(dialogUrl);
    return isTopLevel === true
      && url.searchParams.getAll("mode").length === 1
      && url.searchParams.get("mode") === "popup";
  } catch {
    return false;
  }
}

export function signedAppResources(resources, app) {
  if (!Array.isArray(resources) || !app || typeof app !== "object") {
    throw new Error("Nanocodex Connect received invalid signed application resources.");
  }
  const expectedApp = `${appResourcePrefix}${encodeURIComponent(app.id)}`;
  const expectedOrigin = `${appOriginResourcePrefix}${encodeURIComponent(app.origin)}`;
  const applicationResources = resources.filter((resource) =>
    typeof resource === "string" && resource.startsWith(appResourcePrefix));
  const originResources = resources.filter((resource) =>
    typeof resource === "string" && resource.startsWith(appOriginResourcePrefix));
  if (applicationResources.length !== 1 || applicationResources[0] !== expectedApp
    || originResources.length !== 1 || originResources[0] !== expectedOrigin) {
    throw new Error("The signed application resources do not match this Connect dialog.");
  }
  return resources;
}

export function connectApiOrigin(auth, dialogOrigin) {
  const configured = authEndpoints(auth);
  if (configured.length === 0) {
    throw new Error("Nanocodex Connect has no account broker URL.");
  }
  const origins = configured.map(endpointOrigin);
  if (origins.every((origin) => origin === productionConnectApiOrigin)) {
    return productionConnectApiOrigin;
  }
  if (isLocalDevelopmentOrigin(dialogOrigin)) {
    const expected = origins[0];
    if (!isLocalDevelopmentOrigin(expected) || origins.some((origin) => origin !== expected)) {
      throw new Error("Local Nanocodex Connect auth endpoints must share one development origin.");
    }
    return expected;
  }
  throw new Error("Nanocodex Connect auth endpoints must use the production Connect API.");
}

export function sanitizeWalletResult(result) {
  if (!isRecord(result) || !Array.isArray(result.accounts)) {
    throw new Error("Accounts did not return a connected account.");
  }
  return {
    ...result,
    accounts: result.accounts.map((value) => {
      if (!isRecord(value)) throw new Error("Accounts returned an invalid connected account.");
      const capabilities = isRecord(value.capabilities) ? value.capabilities : {};
      const auth = isRecord(capabilities.auth) ? capabilities.auth : {};
      if (typeof auth.approval_id !== "string" || auth.approval_id.length === 0) {
        throw new Error("Accounts did not return a signed approval identifier.");
      }
      return {
        ...value,
        capabilities: {
          ...capabilities,
          auth: { approval_id: auth.approval_id },
        },
      };
    }),
  };
}

export function appVisibilityPermissions(resources) {
  if (!Array.isArray(resources)) return [];
  const requested = new Set(resources.filter((resource) => typeof resource === "string"));
  const compact = new Set(resources
    .filter((resource) => typeof resource === "string" && resource.startsWith("urn:nanocodex:agent:visibility:"))
    .flatMap((resource) => resource.slice("urn:nanocodex:agent:visibility:".length).split(",")));
  return signedAppVisibility
    .filter(({ resource, name }) => requested.has(resource) || compact.has(name))
    .map(({ name: _name, ...permission }) => permission);
}

export function accountLoginCapabilities(accounts) {
  const credentialIds = Array.isArray(accounts)
    ? [...new Set(accounts.flatMap((account) => {
      const id = isRecord(account) && isRecord(account.credential)
        ? account.credential.id
        : undefined;
      return typeof id === "string" && id.length > 0 ? [id] : [];
    }))]
    : [];
  return credentialIds.length > 0
    ? Object.freeze({ method: "login", credentialId: Object.freeze(credentialIds) })
    : Object.freeze({ method: "login" });
}

export function isLocalDevelopmentOrigin(value) {
  try {
    const url = new URL(value);
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        url.hostname === "localhost"
        || url.hostname === "127.0.0.1"
        || url.hostname === "[::1]"
        || url.hostname === "nanocodex.localhost"
        || url.hostname.endsWith(".nanocodex.localhost")
      );
  } catch {
    return false;
  }
}

export function usesBrowserLocalWebAuthn(value) {
  try {
    const url = new URL(value);
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        url.hostname === "localhost"
        || url.hostname === "127.0.0.1"
        || url.hostname === "[::1]"
      );
  } catch {
    return false;
  }
}

function authEndpoints(auth) {
  if (typeof auth === "string") return [auth];
  if (!isRecord(auth)) return [];
  const endpoints = [];
  for (const name of ["challenge", "url", "verify", "logout"]) {
    if (!(name in auth)) continue;
    if (typeof auth[name] !== "string") {
      throw new Error(`Nanocodex Connect auth ${name} must be a URL.`);
    }
    endpoints.push(auth[name]);
  }
  return endpoints;
}

function endpointOrigin(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("Nanocodex Connect received an invalid auth endpoint.");
  }
  if ((url.protocol !== "https:" && url.protocol !== "http:") || url.username || url.password) {
    throw new Error("Nanocodex Connect received an unsafe auth endpoint.");
  }
  return url.origin;
}

function isAppId(value) {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}

function isSecurePopupOrigin(value) {
  try {
    const url = new URL(value);
    if (url.protocol === "chrome-extension:") {
      return url.href === value && /^[a-p]{32}$/.test(url.hostname);
    }
    return url.origin === value && url.protocol === "https:" && url.hostname.length > 0;
  } catch {
    return false;
  }
}

function originFromUrl(value, error) {
  try {
    const url = new URL(value);
    return url.origin;
  } catch {
    throw new Error(error);
  }
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
