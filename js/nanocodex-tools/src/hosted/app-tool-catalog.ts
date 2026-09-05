import type { HostedToolCatalogEntry } from "./protocol.js";

export function isAppToolCatalogDigest(value: unknown): value is `0x${string}` {
  return typeof value === "string" && /^0x[0-9a-f]{64}$/.test(value);
}

export function hostedToolCatalogEntryAllowed(
  grant: Readonly<{
    grantId: string;
    mcpIds: readonly string[];
    appToolCatalogDigest?: `0x${string}`;
  }> | undefined,
  hostConnectGrantId: string | undefined,
  hostAppToolCatalogDigest: string | undefined,
  entry: HostedToolCatalogEntry,
): boolean {
  if (grant === undefined) return hostConnectGrantId === undefined;
  if (hostConnectGrantId !== grant.grantId) return false;
  const match = /^mcp:([A-Za-z0-9_-]{43})$/.exec(entry.provider);
  if (match !== null) return grant.mcpIds.includes(match[1]!);
  return grant.appToolCatalogDigest !== undefined
    && hostAppToolCatalogDigest === grant.appToolCatalogDigest;
}
