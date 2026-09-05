import {
  createComputerRuntime,
  createWorkspaceFilesystem,
  type ComputerRuntime,
  type ShellFetch,
  type WorkspaceStorageClient,
} from "nanocodex-tools";

import { createCloudflareSshCommand } from "./cloudflare-ssh";
import {
  handleManagedEgress,
  type ManagedEgressConnectorAccess,
  type ManagedEgressConnectorId,
} from "./managed-egress";

type DisposableComputerWorkspace = WorkspaceStorageClient & Readonly<{
  [Symbol.dispose](): void;
}>;

export type ManagedComputerRuntime = ComputerRuntime & Readonly<{
  dispose(): void;
}>;

/** Wires managed persistence, egress, and SSH policy into the generic JS tools. */
export async function createManagedComputerRuntime(options: Readonly<{
  computer: DisposableComputerWorkspace;
  connectorAllowed?: (
    connector: ManagedEgressConnectorId,
    connectionId?: string,
  ) => ManagedEgressConnectorAccess;
  egress: Fetcher;
  sshIdentityAllowed?: (reference: string) => boolean;
  subject?: string;
  sshPassword?: (reference: string) => Promise<string>;
}>): Promise<ManagedComputerRuntime> {
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    options.computer[Symbol.dispose]();
  };

  try {
    const filesystem = await createWorkspaceFilesystem(options.computer);
    const fetch = createManagedShellFetch(
      options.egress,
      options.subject,
      options.connectorAllowed,
    );
    const runtime = await createComputerRuntime({
      filesystem,
      fetch,
      networkMode: options.subject === undefined
        ? "public-http-only"
        : "connector-http-gateway",
      commands: ({ filesystem: mountedFilesystem }) => [{
        name: "ssh",
        load: async () => createCloudflareSshCommand({
          egress: options.egress,
          filesystem: mountedFilesystem,
          ...(options.sshPassword === undefined ? {} : { resolvePassword: options.sshPassword }),
          ...(options.sshIdentityAllowed === undefined
            ? {}
            : { sshIdentityAllowed: options.sshIdentityAllowed }),
          ...(options.subject === undefined ? {} : { subject: options.subject }),
        }),
      }],
    });
    return Object.freeze({
      ...runtime,
      dispose,
      tool: Object.freeze({ ...runtime.tool, dispose }),
    });
  } catch (error) {
    dispose();
    throw error;
  }
}

function createManagedShellFetch(
  binding: Fetcher,
  subject?: string,
  connectorAllowed?: (
    connector: ManagedEgressConnectorId,
    connectionId?: string,
  ) => ManagedEgressConnectorAccess,
): ShellFetch {
  return async (url, options = {}) => {
    const method = (options.method ?? "GET").toUpperCase();
    const request = new Request(url, {
      method,
      headers: options.headers,
      ...(method === "GET" || method === "HEAD" || options.body === undefined
        ? {}
        : { body: options.body }),
      signal: options.signal,
    });
    const response = await handleManagedEgress(request, binding, subject, connectorAllowed);
    const headers: Record<string, string> = Object.create(null) as Record<string, string>;
    response.headers.forEach((value, name) => { headers[name] = value; });
    return {
      status: response.status,
      statusText: response.statusText,
      headers,
      body: new Uint8Array(await response.arrayBuffer()),
      url: response.url || request.url,
    };
  };
}
