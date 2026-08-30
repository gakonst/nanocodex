import { connectActions } from "./Decorator.mjs";
import { iframe } from "./Dialog.mjs";
import { connectionFromWire, connectionMatchesRequest } from "./internal.mjs";
import { create as createRemoteProvider } from "./RemoteProvider.mjs";
import { http } from "./Transport.mjs";

let sequence = 0;

export function create(parameters) {
  if (!parameters || typeof parameters.appId !== "string" || parameters.appId.length === 0) {
    throw new TypeError("Client.create requires appId");
  }
  const transport = parameters.transport ?? http();
  const appOrigin = parameters.appOrigin ?? browserOrigin();
  const dialog = parameters.dialog ?? iframe();
  const transportInstance = transport.setup({ appId: parameters.appId });
  const dialogInstance = dialog.setup({ appId: parameters.appId });
  const identityInstance = parameters.identity?.setup({
    appId: parameters.appId,
    appOrigin,
  });
  const provider = parameters.provider ?? createRemoteProvider({
    host: dialogInstance.host,
    async target(options) {
      await dialogInstance.waitForWallet?.();
      return dialogInstance.walletTarget(options);
    },
  });
  if (!parameters.provider) void provider.prepare().catch(() => undefined);
  const uid = `${transport.key}:${parameters.appId}:${++sequence}`;
  const sessionStorage = parameters.session === false
    ? undefined
    : parameters.session ?? browserSessionStorage();
  const sessionStorageKey = `nanocodex:connect:${parameters.appId}:session`;
  let sessionToken;

  function fetchControlPlane(input, init, token = sessionToken) {
    if (typeof transportInstance.fetch !== "function") {
      throw new TypeError("Connect transport does not expose an HTTP fetch boundary");
    }
    const target = input instanceof Request
      ? input
      : new URL(String(input), transportInstance.baseUrl);
    const targetUrl = new URL(input instanceof Request ? input.url : target);
    if (targetUrl.origin !== new URL(transportInstance.baseUrl).origin) {
      throw new TypeError("Connect client fetch is restricted to its configured API origin");
    }
    const headers = new Headers(
      init?.headers ?? (input instanceof Request ? input.headers : undefined),
    );
    if (token) headers.set("authorization", `Bearer ${token}`);
    return transportInstance.fetch(target, { ...init, headers });
  }

  function requestControlPlane(request, token = sessionToken) {
    return transportInstance.request({
      ...request,
      headers: token
        ? { ...request.headers, authorization: `Bearer ${token}` }
        : request.headers,
    });
  }

  const base = {
    accessKey: parameters.accessKey,
    appId: parameters.appId,
    appOrigin,
    auth: parameters.auth,
    dialog: dialogInstance,
    identity: identityInstance,
    key: parameters.key ?? "connect",
    name: parameters.name ?? "Nanocodex Connect",
    provider,
    fetch: fetchControlPlane,
    request: requestControlPlane,
    transport: Object.freeze({
      key: transport.key,
      name: transport.name,
      type: transport.type,
      baseUrl: transportInstance.baseUrl,
    }),
    type: "connect",
    uid,
  };

  Object.defineProperty(base, "_setSessionToken", {
    enumerable: false,
    value(token) {
      sessionToken = token;
    },
  });
  Object.defineProperty(base, "_setSession", {
    enumerable: false,
    value(session) {
      sessionToken = session.token;
      writeSession(sessionStorage, sessionStorageKey, session);
    },
  });
  Object.defineProperty(base, "_getSession", {
    enumerable: false,
    value() {
      return readSession(sessionStorage, sessionStorageKey);
    },
  });
  Object.defineProperty(base, "_hasSession", {
    enumerable: false,
    value() {
      return readSession(sessionStorage, sessionStorageKey) !== undefined;
    },
  });
  Object.defineProperty(base, "_resumeConnection", {
    enumerable: false,
    value(options) {
      const session = readSession(sessionStorage, sessionStorageKey);
      if (!session?.connection) return undefined;
      try {
        const connection = connectionFromWire(session.connection);
        if (connection.grant.id.toLowerCase() !== session.grantId.toLowerCase()
          || connection.grant.status !== "active"
          || connection.grant.expiresAt <= Math.floor(Date.now() / 1_000)
          || !connectionMatchesRequest(connection, options)) {
          return undefined;
        }
        sessionToken = session.token;
        return connection;
      } catch {
        return undefined;
      }
    },
  });
  Object.defineProperty(base, "_clearSession", {
    enumerable: false,
    value() {
      sessionToken = undefined;
      removeSession(sessionStorage, sessionStorageKey);
    },
  });
  Object.defineProperty(base, "_captureSession", {
    enumerable: false,
    value() {
      const token = sessionToken;
      if (typeof token !== "string" || !token) {
        throw new Error("The Connect grant session is unavailable.");
      }
      return Object.freeze({
        token,
        fetch: (input, init) => fetchControlPlane(input, init, token),
        request: (request) => requestControlPlane(request, token),
      });
    },
  });

  function extend(decorator) {
    if (typeof decorator !== "function") throw new TypeError("client extension must be a function");
    const extension = decorator(client);
    const next = Object.create(Object.getPrototypeOf(client));
    Object.defineProperties(next, Object.getOwnPropertyDescriptors(client));
    return Object.assign(next, extension);
  }

  let client = Object.assign(base, { extend });
  client = client.extend(connectActions());
  return Object.freeze(client);
}

function browserOrigin() {
  try {
    const origin = globalThis.location?.origin;
    return typeof origin === "string" && origin !== "null" ? origin : undefined;
  } catch {
    return undefined;
  }
}

function browserSessionStorage() {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

function readSession(storage, key) {
  if (!storage) return undefined;
  try {
    const value = JSON.parse(storage.getItem(key));
    if (!value
      || typeof value !== "object"
      || !/^0x[0-9a-fA-F]{64}$/.test(value.grantId)
      || typeof value.token !== "string"
      || value.token.length === 0) {
      removeSession(storage, key);
      return undefined;
    }
    return Object.freeze({
      grantId: value.grantId,
      token: value.token,
      ...(value.connection && typeof value.connection === "object"
        ? { connection: value.connection }
        : {}),
    });
  } catch {
    removeSession(storage, key);
    return undefined;
  }
}

function writeSession(storage, key, session) {
  if (!storage) return;
  try {
    storage.setItem(key, JSON.stringify(session));
  } catch {
    // Storage is an optional browser convenience. The in-memory grant session
    // remains valid for the current page when persistence is unavailable.
  }
}

function removeSession(storage, key) {
  if (!storage) return;
  try {
    storage.removeItem(key);
  } catch {
    // A blocked storage implementation is equivalent to an ephemeral session.
  }
}
