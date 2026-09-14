export const CF_SANDBOX_PROVIDER = "cf_sandbox" as const;
export const LEGACY_CF_SANDBOX_PROVIDER = "cloudflare" as const;
export const VM_FACTORY_NAME_PATTERN = /^[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?$/;

const RESERVED_VM_FACTORY_NAMES = new Set<string>([
  CF_SANDBOX_PROVIDER,
  LEGACY_CF_SANDBOX_PROVIDER,
  "host",
]);

export function isVmFactoryName(value: unknown): value is string {
  return typeof value === "string"
    && VM_FACTORY_NAME_PATTERN.test(value)
    && !RESERVED_VM_FACTORY_NAMES.has(value);
}
