export const astraTrialAppId = "astra-one-shot";
export const astraTrialAppOrigin = "https://nanocodex-astra-mpp-trial.gakonst.workers.dev";
export const astraTrialMppLimit = 50_000_000n;

export function hasConsistentAstraTrialIdentity(appId: unknown, origin: unknown): boolean {
  const claimsIdentity = appId === astraTrialAppId || origin === astraTrialAppOrigin;
  return !claimsIdentity || (appId === astraTrialAppId && origin === astraTrialAppOrigin);
}

const MACH = "0x20c000000000000000000000f37de3740adec032";
const ZERO_ADDRESS = "0x0000000000000000000000000000000000000000";
const PERIOD = 86_400;
const TRANSFER_WITH_MEMO = "0x95777d59";

export function hasAstraTrialSpendPolicy(limits: unknown[], scopes: unknown[]): boolean {
  if (limits.length !== 1 || scopes.length !== 1) return false;
  const limit = record(limits[0]);
  const scope = record(scopes[0]);
  if (!limit || !scope || !Array.isArray(scope.recipients) || scope.recipients.length !== 1) {
    return false;
  }
  const recipient = address(scope.recipients[0]);
  return address(limit.token) === MACH
    && limit.limit === astraTrialMppLimit.toString()
    && limit.period === PERIOD
    && address(scope.address) === MACH
    && typeof scope.selector === "string"
    && scope.selector.toLowerCase() === TRANSFER_WITH_MEMO
    && recipient !== undefined
    && recipient !== ZERO_ADDRESS;
}

function address(value: unknown): string | undefined {
  return typeof value === "string" && /^0x[0-9a-fA-F]{40}$/.test(value)
    ? value.toLowerCase()
    : undefined;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}
