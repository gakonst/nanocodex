import { fundResultFromWire, machineUsdConfigFromWire } from "../internal.mjs";

export async function getConfig(client, options = {}) {
  return machineUsdConfigFromWire(await client.request({
    method: "GET",
    path: "/v1/machine-usd/config",
    signal: options.signal,
  }));
}

export async function fund(client, options) {
  if (!options?.grantId) throw new TypeError("fund requires grantId");
  if (!Number.isSafeInteger(options.usdAmountCents)) {
    throw new TypeError("fund requires an integer usdAmountCents");
  }
  const config = await getConfig(client, { signal: options.signal });
  if (!config.onrampEnabled) throw new Error("The MACH onramp is unavailable");
  if (options.usdAmountCents < config.minUsdAmountCents || options.usdAmountCents > config.maxUsdAmountCents) {
    throw new RangeError(`MACH amount must be from ${config.minUsdAmountCents} through ${config.maxUsdAmountCents} cents`);
  }
  const approval = await client.dialog.open(Object.freeze({
    id: crypto.randomUUID(),
    type: "machineUsdFund",
    grantId: options.grantId,
    accountAddress: options.accountAddress,
    usdAmountCents: options.usdAmountCents,
    tokenAddress: config.tokenAddress,
    chainId: config.chainId,
    apiUrl: client.transport.baseUrl.replace(/\/+$/, ""),
    stripePublishableKey: config.stripePublishableKey,
  }));
  return fundResultFromWire(await client.request({
    method: "GET",
    path: `/v1/grants/${options.grantId}/mpp/balance`,
    signal: options.signal,
  }).then((connection) => ({ order: approval.order, connection })), client);
}
