/** Native execution is an independent Connect permission, never agent.run. */
export type ExecutionAuthorization = Readonly<{
  capabilities: readonly string[];
  connectGrant?: Readonly<{ grantId: string; sandboxExecution?: boolean }>;
}>;

type ExecutionMount = Readonly<{ provider: string; configuration_json: string }>;

export function turnCanUseExecutionNamespace(authorization: ExecutionAuthorization | undefined): boolean {
  return authorization !== undefined
    && (authorization.connectGrant === undefined || authorization.connectGrant.sandboxExecution === true)
    && authorization.capabilities.includes("agents:write")
    && authorization.capabilities.includes("tools:use");
}

export function turnCanProvisionExecutionProvider(authorization: ExecutionAuthorization | undefined, provider: string): boolean {
  return turnCanUseExecutionNamespace(authorization)
    && (authorization!.connectGrant === undefined || provider === "cf_sandbox");
}

/** Undefined is a legacy/account mount; invalid retained owners fail closed. */
export function executionMountOwner(mount: ExecutionMount): string | undefined | null {
  try {
    const configuration = JSON.parse(mount.configuration_json);
    if (!configuration || typeof configuration !== "object" || Array.isArray(configuration)) return null;
    const owner = configuration.connect_grant_id;
    return owner === undefined || (typeof owner === "string" && /^0x[0-9a-f]{64}$/.test(owner)) ? owner : null;
  } catch { return null; }
}

export function executionMountAllowed(authorization: ExecutionAuthorization | undefined, mount: ExecutionMount): boolean {
  if (!turnCanUseExecutionNamespace(authorization)) return false;
  const owner = executionMountOwner(mount);
  return owner !== null && owner === authorization!.connectGrant?.grantId
    && (owner === undefined || mount.provider === "cloudflare");
}

/** FUSE permissions belong to the executing mount, never the current root turn. */
export function executionMountPeers<T extends ExecutionMount>(source: ExecutionMount, mounts: readonly T[]): T[] {
  const owner = executionMountOwner(source);
  return owner === null ? [] : mounts.filter(mount => mount.provider === "cloudflare" && executionMountOwner(mount) === owner);
}
