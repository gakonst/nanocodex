import {
  createSshCommand,
  createWebStreamSshStream,
  type SshCommandResult,
  type SshIdentityReferenceRequest,
} from "../tools/ssh.mjs";
import type { Workspace } from "../tools/types.mjs";

export type SshSocket = Readonly<{
  readable: ReadableStream<Uint8Array>;
  writable: WritableStream<Uint8Array>;
  opened: Promise<unknown>;
  closed: Promise<void>;
  close(): Promise<void>;
}>;

export type SshConnect = (
  address: Readonly<{ hostname: string; port: number }>,
  options: Readonly<{ allowHalfOpen: boolean; secureTransport: "off" }>,
) => SshSocket;

/** Creates a workspace-backed SSH command over an injected Web Streams socket transport. */
export function createWorkspaceSshCommand(options: Readonly<{
  connect: SshConnect;
  filesystem(): Workspace;
  resolvePassword?(reference: string): Promise<string>;
  executeWithIdentityReference?(
    request: SshIdentityReferenceRequest,
    context: Readonly<{ signal?: AbortSignal }>,
  ): Promise<SshCommandResult>;
}>) {
  return createSshCommand({
    transport: "tcp",
    maxOutputBytes: 4 * 1024 * 1024,
    readIdentity: async (path, context) => new TextDecoder().decode(
      await options.filesystem().readFile(resolveWorkspacePath(
        options.filesystem().root,
        context.cwd,
        path,
      )),
    ),
    ...(options.resolvePassword === undefined
      ? {}
      : { resolvePassword: options.resolvePassword }),
    ...(options.executeWithIdentityReference === undefined
      ? {}
      : { executeWithIdentityReference: options.executeWithIdentityReference }),
    async openStream(endpoint, signal) {
      if (endpoint instanceof URL) throw new Error("TCP SSH requires a host and port");
      const socket = options.connect(endpoint, { allowHalfOpen: true, secureTransport: "off" });
      try {
        await abortable(socket.opened, signal);
        return createWebStreamSshStream(socket, signal);
      } catch (error) {
        await socket.close();
        throw error;
      }
    },
  });
}

function resolveWorkspacePath(root: string, cwd: string, path: string): string {
  const absolute = path.startsWith("/");
  if (absolute && path !== root && !path.startsWith(`${root}/`)) {
    throw new Error(`SSH identity path must stay within ${root}`);
  }
  const parts = absolute ? [] : relativeParts(root, cwd);
  const input = absolute ? path.slice(root.length) : path;
  for (const part of input.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (parts.length === 0) throw new Error(`SSH identity path escapes ${root}`);
      parts.pop();
    } else parts.push(part);
  }
  return parts.length === 0 ? root : `${root}/${parts.join("/")}`;
}

function relativeParts(root: string, path: string): string[] {
  if (path === root) return [];
  if (!path.startsWith(`${root}/`)) throw new Error(`SSH cwd must stay within ${root}`);
  return path.slice(root.length + 1).split("/").filter(Boolean);
}

function abortable<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (signal === undefined) return promise;
  if (signal.aborted) return Promise.reject(signal.reason ?? new Error("SSH command cancelled"));
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason ?? new Error("SSH command cancelled"));
    signal.addEventListener("abort", abort, { once: true });
    promise.then(
      (value) => { signal.removeEventListener("abort", abort); resolve(value); },
      (error) => { signal.removeEventListener("abort", abort); reject(error); },
    );
  });
}
