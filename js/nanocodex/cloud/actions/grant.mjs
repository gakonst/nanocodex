import { grantFromWire } from "../internal.mjs";

export async function revoke(client, options) {
  if (!options?.grantId) throw new TypeError("revoke requires grantId");
  const grant = grantFromWire(await client.request({
    method: "POST",
    path: `/v1/grants/${options.grantId}/revoke`,
    signal: options.signal,
  }));
  client._clearSession();
  return grant;
}
