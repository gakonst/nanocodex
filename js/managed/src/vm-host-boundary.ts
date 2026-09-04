export const VM_HOST_POOL_SCOPE = "x-nanocodex-pool-scope";
export const VM_HOST_POOL_OWNER = "x-nanocodex-pool-owner";
export const VM_HOST_POOL_AGENT = "x-nanocodex-pool-agent";
export const VM_HOST_DONOR = "x-nanocodex-donor-id";
export const VM_HOST_PUBLIC_ORIGIN = "x-nanocodex-public-origin";
export const VM_HOST_POOL_LOCATOR = "x-nanocodex-pool-locator";

export const VM_HOST_ATTACHMENT_ROUTE =
  /^vm-host:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}:[1-9][0-9]{0,15}$/;

export function vmHostAttachmentRouteId(allocationId: string, hostEpoch: number): string {
  return `vm-host:${allocationId}:${hostEpoch}`;
}
