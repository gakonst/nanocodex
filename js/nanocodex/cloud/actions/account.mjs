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
