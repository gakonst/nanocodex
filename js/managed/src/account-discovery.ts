import { consumeRpcData } from "nanocodex/cloudflare/rpc";
import type { CloudflareAccountDiscoveryOptions, CloudflareAccountMetadataBinding, CloudflareAccountMetadataComponent } from "nanocodex/cloudflare/egress";

export const ACCOUNT_DISCOVERY_TTL_MS = 15 * 60_000;
export type DiscoveryRead = CloudflareAccountDiscoveryOptions & Readonly<{ adoptExpiry(expiresAt: number): void }>;

/** Call only after feature detection. An RPC error never triggers a live retry. */
export async function discoveryMetadata(
  binding: CloudflareAccountMetadataBinding,
  userId: string,
  component: CloudflareAccountMetadataComponent,
  discovery: DiscoveryRead,
): Promise<unknown> {
  const result = consumeRpcData(await Reflect.apply(binding.readAccountDiscovery!, binding, [
    userId, component, { authorityKey: discovery.authorityKey, reload: discovery.reload },
  ]));
  if (result.status !== 200) throw new Error(`account ${component} failed with HTTP ${result.status}`);
  if (result.schema !== 1 || !Number.isSafeInteger(result.expiresAt) || result.expiresAt < 0
    || result.expiresAt > Date.now() + ACCOUNT_DISCOVERY_TTL_MS) {
    throw new Error(`account ${component} returned invalid discovery expiry`);
  }
  discovery.adoptExpiry(result.expiresAt);
  return result.data;
}
