import { Sandbox } from "@vercel/sandbox";

export const PHYSICAL_WORKSPACE = "/vercel/sandbox";
export const VIRTUAL_WORKSPACE = "/workspace";
export const PREVIEW_PORTS = [3000, 5173, 8000, 8080] as const;

export function sessionSandboxName(sessionId: string): string {
  return `nanocodex-${sessionId}`;
}

export async function prepareSessionSandbox(sessionId: string): Promise<Sandbox> {
  const sandbox = await Sandbox.getOrCreate({
    name: sessionSandboxName(sessionId),
    runtime: "node24",
    persistent: true,
    timeout: 10 * 60_000,
    ports: [...PREVIEW_PORTS],
    keepLastSnapshots: { count: 3, expiration: 7 * 24 * 60 * 60_000 },
    tags: { application: "nanocodex", session: sessionId.slice(0, 64) },
  });
  const linked = await sandbox.runCommand({
    cmd: "bash",
    args: [
      "-lc",
      `if [ -e ${VIRTUAL_WORKSPACE} ] || [ -L ${VIRTUAL_WORKSPACE} ]; then test "$(readlink -f ${VIRTUAL_WORKSPACE})" = ${PHYSICAL_WORKSPACE}; else ln -s ${PHYSICAL_WORKSPACE} ${VIRTUAL_WORKSPACE}; fi`,
    ],
    sudo: true,
    timeoutMs: 10_000,
  });
  if (linked.exitCode !== 0) {
    throw new Error(`failed to prepare /workspace: ${await linked.stderr()}`);
  }
  return sandbox;
}
