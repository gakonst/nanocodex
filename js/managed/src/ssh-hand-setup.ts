import { serverHandID } from "./hand-hosts";
import type { NamedTool, ToolContext } from "nanocodex";

const REFERENCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const IMAGE = /^[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}$/;
type Identity = { reference: string; hostname: string; port: number; username: string; host_key_sha256: string };
type SetupOptions = {
  owner: string;
  subject: string;
  origin: string;
  image?: string;
  egress: Fetcher;
  hosts: Fetcher;
  authorize(context: ToolContext): void;
};

/** SSH is used for enrollment; media/input then use the normal Hand connection. */
export function serverHandTool(options: SetupOptions): NamedTool {
  return {
    name: "server_hand",
    description: "List vault SSH targets, connect a Linux server as an interactive Hand, or disconnect it. Use only for a server the user asked to connect. Connect installs a dedicated desktop container over the target-bound vault SSH identity; the server must provide Docker access. Keys and Hand credentials stay outside the transcript. Reconnect reuses its machine identity and workspace. A published screen is not proof of decoded video; verify with its screen tool or viewer.",
    parameters: { type: "object", properties: {
      operation: { type: "string", enum: ["list", "connect", "disconnect"] },
      identity_ref: { type: "string", description: "Exact SSH reference from list; required for connect/disconnect." },
    }, required: ["operation"], additionalProperties: false },
    handler: async (input, context) => {
      options.authorize(context);
      if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("server_hand input must be an object");
      const value = input as Record<string, unknown>;
      if (Object.keys(value).some(key => !["operation", "identity_ref"].includes(key))
        || !["list", "connect", "disconnect"].includes(String(value.operation))) throw new TypeError("invalid server_hand operation");
      const reference = value.identity_ref;
      if (value.operation !== "list" && (typeof reference !== "string" || !REFERENCE.test(reference))) throw new TypeError("an exact SSH identity reference is required");
      const signal = AbortSignal.any([context.signal, AbortSignal.timeout(90_000)]);
      const status = await options.egress.fetch(`https://broker.internal/users/${encodeURIComponent(options.owner)}/credentials`, { signal });
      if (!status.ok) throw new Error("SSH target metadata is unavailable");
      const body = await status.json<{ ssh?: Identity[] }>();
      const identities = body.ssh ?? [];
      if (value.operation === "list") {
        return { targets: identities.map(({ reference, hostname, port, username, host_key_sha256 }) => ({
          reference, hostname, port, username, host_key_sha256,
        })), installation_available: Boolean(options.image && IMAGE.test(options.image)) };
      }
      const identity = identities.find(identity => identity.reference === reference);
      if (!identity) throw new Error("The selected SSH identity is not in this account's vault");
      const id = await serverHandID(options.owner, identity.reference);
      const machineID = `server:${id}`;
      const ownerHeaders = { "x-nanocodex-owner-id": options.owner };
      const manage = (method: string, body?: unknown, cleanup = false) => options.hosts.fetch(`https://account-tools.internal/hand-hosts/${id}`, {
        method, headers: ownerHeaders, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        signal: cleanup ? AbortSignal.timeout(5000) : signal,
      });
      const ssh = async (command: string[], stdin?: string, cleanup = false) => {
        const response = await options.egress.fetch("https://ssh.internal/v1/execute", {
          method: "POST", headers: { "content-type": "application/json", "x-nanocodex-subject": options.subject },
          body: JSON.stringify({ identity_ref: identity.reference, hostname: identity.hostname,
            port: identity.port, username: identity.username, command, ...(stdin === undefined ? {} : { stdin }) }),
          signal: cleanup ? AbortSignal.timeout(5000) : signal,
        });
        if (!response.ok) { await response.body?.cancel(); throw new Error("The vault SSH connection failed; check its target, host fingerprint, and authorized public key"); }
        const result = await response.json<{ exit_code?: number }>();
        // Remote output can contain secrets or instructions. Only the exit code
        // crosses this operation's model-visible boundary.
        if (!Number.isSafeInteger(result.exit_code)) throw new Error("Invalid SSH setup result");
        return result.exit_code!;
      };
      const operation = crypto.randomUUID();
      const lock = (method: string) => options.hosts.fetch(`https://account-tools.internal/hand-host-setups/${id}`, {
        method, headers: ownerHeaders, body: JSON.stringify({ operation_id: operation }),
        signal: method === "DELETE" ? AbortSignal.timeout(5000) : signal,
      });
      const locked = await lock("POST");
      if (!locked.ok) { await locked.body?.cancel(); throw new Error("This server already has a setup operation in progress"); }
      let enrolled = false;
      try {
        if (value.operation === "disconnect") {
          const revoked = await manage("DELETE");
          if (!revoked.ok) throw new Error("Could not revoke the server Hand");
          const exit = await ssh(["sh", "-c", SERVER_HAND_STOP, "nanocodex-hand", id]);
          return { machine_id: machineID, status: "revoked", service_stopped: exit === 0, workspace_retained: true };
        }
        if (!options.image || !IMAGE.test(options.image)) throw new Error("Server Hand installation requires an operator-configured image pinned by digest");
        // Check Docker before rotating credentials or changing a working Hand.
        if (await ssh(["sh", "-c", "test \"$(uname -s)\" = Linux && command -v docker >/dev/null && docker info >/dev/null 2>&1"]) !== 0)
          throw new Error("The selected SSH user needs access to Docker on this Linux server");
        const label = `${identity.username}@${identity.hostname}`.slice(0, 128);
        const response = await manage("PUT", { name: label });
        if (!response.ok) { await response.body?.cancel(); throw new Error("Could not enroll the server Hand"); }
        enrolled = true;
        const receipt = await response.json<{ credential: string }>();
        const endpoint = `${new URL(options.origin).origin}/v1/hand-hosts/${options.owner}/${id}/hands`;
        const exit = await ssh(["sh", "-c", SERVER_HAND_INSTALL, "nanocodex-hand", id, endpoint,
          label, options.image], receipt.credential + "\n");
        if (exit !== 0) throw new Error(`Server Hand installation failed (exit ${exit})`);
        const deadline = Date.now() + 20_000;
        do {
          signal.throwIfAborted();
          const response = await options.hosts.fetch("https://account-tools.internal/hands/screens", { headers: ownerHeaders, signal });
          if (response.ok) {
            const catalog = await response.json<{ surfaces: { machine_id: string; id: string }[] }>();
            if (catalog.surfaces.some(surface => surface.machine_id === machineID && surface.id === "desktop")) {
              enrolled = false;
              return { machine_id: machineID, status: "published", workspace_retained: true };
            }
          } else await response.body?.cancel();
          await new Promise(resolve => setTimeout(resolve, 500));
        } while (Date.now() < deadline);
        throw new Error("The server started but did not publish a screen");
      } finally {
        let cleanupFailed = false;
        if (enrolled) {
          try { cleanupFailed = !(await manage("DELETE", undefined, true)).ok; } catch { cleanupFailed = true; }
          await ssh(["sh", "-c", SERVER_HAND_STOP, "nanocodex-hand", id], undefined, true).catch(() => {});
        }
        await lock("DELETE").catch(() => {});
        if (cleanupFailed) throw new Error("Server setup failed and credential revocation could not be confirmed; disconnect this server to retry cleanup");
      }
    },
  };
}

export { serverHandID } from "./hand-hosts";

// The credential enters via SSH stdin. Arguments, Docker configuration, and
// logs contain its file path, never the credential itself. No ports are exposed.
export const SERVER_HAND_INSTALL = String.raw`set -eu
umask 077
id="$1"; endpoint="$2"; label="$3"; image="$4"
state="$(printenv XDG_STATE_HOME || true)"
test -n "$state" || state="$HOME/.local/state"
state="$state/nanocodex/hands/$id"
container="nanocodex-hand-$id"
test ! -L "$state"
mkdir -p "$state"
chmod 700 "$state"
test ! -L "$state/workspace" && test ! -L "$state/credential"
mkdir -p "$state/workspace"
IFS= read -r credential
test "$(printf '%s' "$credential" | wc -c)" -eq 43
if docker container inspect "$container" >/dev/null 2>&1; then
  test "$(docker inspect --format '{{index .Config.Labels "nanocodex.hand.id"}}' "$container")" = "$id"
fi
docker image inspect "$image" >/dev/null 2>&1 || docker pull "$image" >/dev/null
temporary="$(mktemp "$state/credential.XXXXXX")"
trap 'rm -f "$temporary"' EXIT
printf '%s\n' "$credential" > "$temporary"
chmod 600 "$temporary"
mv "$temporary" "$state/credential"
trap - EXIT
unset credential
if docker container inspect "$container" >/dev/null 2>&1; then docker rm -f "$container" >/dev/null; fi
docker run -d --name "$container" --label "nanocodex.hand.id=$id" --restart unless-stopped --init \
  --cap-drop ALL --security-opt no-new-privileges --pids-limit 256 --memory 2g \
  --user "$(id -u):$(id -g)" --env XDG_CONFIG_HOME=/state/config --env XDG_CACHE_HOME=/state/cache \
  --mount "type=bind,src=$state,dst=/state" --mount "type=bind,src=$state/workspace,dst=/workspace" \
  --entrypoint /usr/local/bin/nanocodex-remote "$image" server-host --url "$endpoint" \
  --credential-file /state/credential --machine-id "server:$id" --name "$label" --workspace /workspace >/dev/null
`;

const SERVER_HAND_STOP = String.raw`set -eu
id="$1"; container="nanocodex-hand-$id"
if docker container inspect "$container" >/dev/null 2>&1; then
  test "$(docker inspect --format '{{index .Config.Labels "nanocodex.hand.id"}}' "$container")" = "$id"
  docker rm -f "$container" >/dev/null
fi
`;
