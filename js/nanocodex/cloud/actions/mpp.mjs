import { chargeResultFromWire, connectionFromWire } from "../internal.mjs";

export async function getBalance(client, options) {
  if (!options?.grantId) throw new TypeError("getBalance requires grantId");
  const wire = await client.request({
    method: "GET",
    path: `/v1/grants/${options.grantId}/mpp/balance`,
    signal: options.signal,
  });
  const connection = connectionFromWire(wire);
  const session = client._getSession?.();
  if (session && session.grantId.toLowerCase() === connection.grant.id.toLowerCase()) {
    const { grant_token: _grantToken, ...sessionConnection } = wire;
    client._setSession({
      grantId: session.grantId,
      token: session.token,
      connection: sessionConnection,
    });
  }
  return connection;
}

export async function charge(client, options) {
  if (!options?.grantId) throw new TypeError("charge requires grantId");
  if (typeof options.amount !== "bigint" || options.amount <= 0n) {
    throw new TypeError("charge requires a positive bigint amount");
  }
  const origin = new URL(options.origin).origin;
  return chargeResultFromWire(await client.request({
    method: "POST",
    path: `/v1/grants/${options.grantId}/mpp/charge`,
    body: {
      amount_atomics: String(options.amount),
      origin,
      memo: options.memo,
    },
    signal: options.signal,
  }), client);
}
