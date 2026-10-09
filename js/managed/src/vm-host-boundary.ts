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

/** Device-credential binding of an account VM factory socket; set only by the Worker. */
export const VM_HOST_DEVICE_ID = "x-nanocodex-vm-host-device-id";
export const VM_HOST_DEVICE_MACHINE = "x-nanocodex-vm-host-device-machine";
export const VM_HOST_DEVICE_KEY_VERSION = "x-nanocodex-vm-host-device-key-version";

export type VmHostDevice = Readonly<{ device_id: string; machine_id: string; key_version: number }>;

/**
 * Sets the authenticated device binding, or removes every device header so a
 * caller cannot assert one on an account-credential connection.
 */
export function setVmHostDevice(headers: Headers, device: VmHostDevice | undefined): void {
  for (const name of [VM_HOST_DEVICE_ID, VM_HOST_DEVICE_MACHINE, VM_HOST_DEVICE_KEY_VERSION]) headers.delete(name);
  if (device === undefined) return;
  headers.set(VM_HOST_DEVICE_ID, device.device_id);
  headers.set(VM_HOST_DEVICE_MACHINE, device.machine_id);
  headers.set(VM_HOST_DEVICE_KEY_VERSION, String(device.key_version));
}

/** Durable object name of a scope's VM host pool. */
export async function vmHostPoolLocator(scope: "agent" | "account" | "system", identity: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`nanocodex:vm-host-pool:v1\0${scope}\0${identity}`),
  ));
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/u, "");
}
