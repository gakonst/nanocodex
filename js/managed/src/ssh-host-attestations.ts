import { WorkerEntrypoint } from "cloudflare:workers";
import { serverHandID } from "./hand-hosts";

const OWNER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const REFERENCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const TRUST = /^(?:device|hand:[A-Za-z0-9][A-Za-z0-9._:-]{0,127})$/;
const headers = { "cache-control": "no-store" };

/** The account DO's ACTIVE current-key device attestation for one machine. */
export type DeviceSshHostKeys = {
  device_id: string;
  created_at: number;
  key_version: number;
  host_keys: { fingerprint: string; attested_at: number }[];
} | null;

type AttestationSource = {
  deviceSshHostKeys(ownerId: string, machineId: string): Promise<DeviceSshHostKeys>;
};

type Env = { NANOCODEX_ACCOUNT_TOOLS: { getByName(name: string): unknown } };

/**
 * Machine whose device may vouch for a Vault SSH target's host key. A
 * `device` binding is the reference's own server Hand; `hand:MACHINE` is
 * the owner-saved exact Hand. Never derived from a hostname.
 */
export async function attestationMachine(owner: string, reference: string, trust: string): Promise<string> {
  return trust === "device" ? `server:${await serverHandID(owner, reference)}` : trust.slice("hand:".length);
}

/**
 * Private service-binding entrypoint for the egress SSH broker. Egress calls it
 * only after resolving the owner from the agent subject and the Vault target
 * from that owner's credential broker; it is absent from public HTTP routing.
 */
export class HandDeviceSshHostKeys extends WorkerEntrypoint<Env> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.hostname !== "hand-device-ssh.internal"
      || url.pathname !== "/v1/host-keys" || url.search) return Response.json({ error: "not_found" }, { status: 404, headers });
    let value: unknown;
    try { value = await request.json(); } catch { value = undefined; }
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 3) {
      return Response.json({ error: "invalid_request" }, { status: 400, headers });
    }
    const { owner_id: owner, reference, trust } = value as Record<string, unknown>;
    if (typeof owner !== "string" || !OWNER.test(owner) || typeof reference !== "string" || !REFERENCE.test(reference)
      || typeof trust !== "string" || !TRUST.test(trust)) {
      return Response.json({ error: "invalid_request" }, { status: 400, headers });
    }
    const machine = await attestationMachine(owner, reference, trust);
    // Read at call time from the owner's own DO: revocation, rotation and
    // forget take effect on the next SSH call. No cache.
    const source = this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner) as AttestationSource;
    const device = await source.deviceSshHostKeys(owner, machine);
    return Response.json({ attestation: device === null ? null : {
      device_id: device.device_id,
      created_at: device.created_at,
      host_keys: device.host_keys.map(({ fingerprint, attested_at }) => ({ fingerprint, attested_at })),
    } }, { headers });
  }
}
