import type { Sandbox } from "@cloudflare/sandbox";
import { HAND_HOST_ID } from "./hand-hosts";

export type SandboxDesktopScope = { owner: string; id: string; machineId: string; name: string };
export type SandboxHandHosts = { getByName(name: string): Pick<DurableObjectStub, "fetch"> };
type State = SandboxDesktopScope & { credential?: string; expiresAt?: number; processId?: string };
type Runtime = Pick<Sandbox, "exec" | "writeFile" | "getProcess" | "startProcess" | "killProcess">;
const KEY = "nanocodex-desktop";
const DIRECTORY = "/run/nanocodex-hand";

/** Trusted lifecycle glue. No account or provider credential enters the guest. */
export class SandboxDesktop {
  private pending?: Promise<void>;
  constructor(private storage: DurableObjectStorage, private runtime: Runtime,
    private hosts: SandboxHandHosts, private bind: (scope: SandboxDesktopScope) => Promise<void>) {}

  async configure(scope: SandboxDesktopScope): Promise<void> {
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(scope.owner) || !HAND_HOST_ID.test(scope.id)
      || !/^cf:[A-Za-z0-9][A-Za-z0-9._:-]{0,120}$/.test(scope.machineId)
      || !scope.name || new TextEncoder().encode(scope.name).length > 128 || /[\u0000-\u001f\u007f]/.test(scope.name)) throw new Error("invalid sandbox desktop scope");
    await this.storage.transaction(async storage => {
      const current = await storage.get<State>(KEY);
      if (current && (current.owner !== scope.owner || current.id !== scope.id || current.machineId !== scope.machineId)) throw new Error("sandbox desktop belongs to another mount");
      if (!current) await storage.put(KEY, scope);
    });
    await this.ensure();
  }

  ensure(): Promise<void> {
    if (!this.pending) this.pending = this.start().finally(() => { this.pending = undefined; });
    return this.pending;
  }

  async clear(): Promise<void> {
    await this.pending?.catch(() => {});
    const state = await this.storage.get<State>(KEY);
    if (!state) return;
    const response = await this.manage(state, "DELETE");
    if (!response.ok) throw new Error("could not revoke sandbox desktop");
    await this.storage.delete(KEY);
    if (state.processId) await this.runtime.killProcess(state.processId).catch(() => {});
  }

  private async start(): Promise<void> {
    const state = await this.storage.get<State>(KEY);
    if (!state) return;
    await this.bind({ owner: state.owner, id: state.id, machineId: state.machineId, name: state.name });
    const process = state.processId ? await this.runtime.getProcess(state.processId) : null;
    const running = process && ["running", "starting"].includes(process.status);
    if (running && state.credential && (state.expiresAt ?? 0) > Date.now() + 86_400_000) return;
    // Check the built image before issuing or rotating authority.
    // Cloudflare creates /dev at runtime without /dev/shm. wlroots uses
    // POSIX shared-memory files for keyboard maps, so image-time setup is lost.
    const prepared = await this.runtime.exec(`test -x /usr/local/bin/nanocodex-remote && test ! -L /dev/shm && install -d -m 1777 /dev/shm && test ! -L ${DIRECTORY} && install -d -m 0700 ${DIRECTORY}`, { cwd: "/workspace" });
    if (!prepared.success) throw new Error("sandbox desktop image is unavailable");
    if (!state.credential || (state.expiresAt ?? 0) <= Date.now() + 86_400_000) {
      const response = await this.manage(state, "PUT");
      if (!response.ok) throw new Error("could not enroll sandbox desktop");
      const receipt = await response.json<{ credential: string; expires_at: number }>();
      if (!/^[A-Za-z0-9_-]{43}$/.test(receipt.credential) || !Number.isSafeInteger(receipt.expires_at)
        || receipt.expires_at <= Date.now()) throw new Error("invalid sandbox desktop enrollment");
      state.credential = receipt.credential; state.expiresAt = receipt.expires_at;
      await this.storage.put(KEY, state);
    }
    // Avoid world-readable creation even briefly. The parent directory is 0700.
    const file = await this.runtime.writeFile(`${DIRECTORY}/credential.next`, state.credential + "\n");
    if (!file.success) throw new Error("could not write sandbox desktop credential");
    const written = await this.runtime.exec(`chmod 0600 ${DIRECTORY}/credential.next && mv -f ${DIRECTORY}/credential.next ${DIRECTORY}/credential`, { cwd: "/workspace" });
    if (!written.success) throw new Error("could not install sandbox desktop credential");
    if (running) return;
    state.processId = `nanocodex-desktop-${crypto.randomUUID()}`;
    await this.storage.put(KEY, state);
    const endpoint = `https://nanocodex-hand.internal/v1/hand-hosts/${state.owner}/${state.id}/hands`;
    // All values are trusted, validated metadata and shell-quoted. The private
    // token only appears in writeFile's payload, never argv or process logs.
    const args = ["/usr/local/bin/nanocodex-remote", "server-host", "--frames", "--url", endpoint,
      "--credential-file", `${DIRECTORY}/credential`, "--machine-id", state.machineId,
      "--name", state.name, "--workspace", "/workspace"];
    await this.runtime.startProcess(args.map(shellQuote).join(" "), {
      cwd: "/workspace", processId: state.processId, autoCleanup: true,
    });
  }

  private manage(state: State, method: string) {
    return this.hosts.getByName(state.owner).fetch(`https://account-tools.internal/sandbox-hand-hosts/${state.id}`, {
      method, headers: { "x-nanocodex-owner-id": state.owner },
      ...(method === "PUT" ? { body: JSON.stringify({ name: state.name, machine_id: state.machineId }) } : {}),
    });
  }
}

function shellQuote(value: string) { return "'" + value.replaceAll("'", "'\\''") + "'"; }
