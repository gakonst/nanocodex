/** Signs out the Nanocodex account without revoking its app grant or access key. */
export async function logout(client) {
  client._clearSession();
  let failure;
  if (!client.principal) {
    try {
      await client.provider.request({ method: "wallet_disconnect" });
    } catch (error) {
      failure = error;
    }
  }
  const cleanup = await Promise.allSettled([
    client.provider.reset?.(),
    client.dialog.resetWallet?.(),
  ]);
  if (failure) throw failure;
  const cleanupFailure = cleanup.find((result) => result.status === "rejected");
  if (cleanupFailure?.status === "rejected") throw cleanupFailure.reason;
}

/** Discovers ordinary account navigation URLs; opening them uses browser sign-in. */
export function links(client, options = {}) {
  const query = new URLSearchParams();
  for (const key of Object.keys(options)) {
    if (key !== "connect" && key !== "add") throw new TypeError(`Unknown account links option: ${key}`);
    if (options[key] !== undefined) query.set(key, options[key]);
  }
  const suffix = query.size ? `?${query}` : "";
  return client.request({ method: "GET", path: `/v1/account/links${suffix}` });
}
