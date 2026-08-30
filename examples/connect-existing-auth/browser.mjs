import { Client, Identity } from "../../js/bindings/cloud/index.mjs";

/** The browser setup is identical for Auth0, Better Auth, and Privy. */
export function createConnectClient(parameters = {}) {
  return Client.create({
    appId: parameters.appId ?? "existing-auth-example",
    appOrigin: parameters.appOrigin,
    identity: Identity.host({
      url: parameters.sessionUrl ?? "/api/nanocodex/session",
      ...(parameters.fetch ? { fetch: parameters.fetch } : {}),
    }),
    ...(parameters.dialog ? { dialog: parameters.dialog } : {}),
    ...(parameters.provider ? { provider: parameters.provider } : {}),
    ...(parameters.transport ? { transport: parameters.transport } : {}),
  });
}
