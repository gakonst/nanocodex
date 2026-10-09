import { DurableObject } from "cloudflare:workers";
export { HandDeviceSshHostKeys } from "../../../managed/src/ssh-host-attestations.ts";

// Stands in for AccountHostedTools.deviceSshHostKeys: per machine, the ACTIVE
// current-key device attestation, or null (unknown, revoked or forgotten).
export class AccountHostedTools extends DurableObject {
  async deviceSshHostKeys(_owner, machineId) {
    const devices = (await this.ctx.storage.get("devices")) ?? {};
    return Object.hasOwn(devices, machineId) ? devices[machineId] : null;
  }
  async fetch(request) {
    await this.ctx.storage.put("devices", await request.json());
    return new Response(null, { status: 204 });
  }
}

export default {
  fetch(request, env) {
    const owner = new URL(request.url).searchParams.get("owner");
    return env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).fetch(request);
  },
};
