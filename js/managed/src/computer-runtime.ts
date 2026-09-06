import {
  createComputerRuntime,
  createWorkspaceFilesystem,
  type ComputerRuntime,
  type ShellFetch,
  type Workspace,
  type WorkspaceStorageClient,
} from "nanocodex-tools";
import { AsyncLocalStorage } from "node:async_hooks";
import type { ToolContext } from "nanocodex";

import { createCloudflareSshCommand } from "./cloudflare-ssh";
import {
  handleManagedEgress,
  VAULT_ID_HEADER,
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
  filesystem?: Workspace;
  connectorAllowed?: (
    connector: ManagedEgressConnectorId,
    connectionId?: string,
    context?: ToolContext,
  ) => ManagedEgressConnectorAccess;
  egress: Fetcher;
  sshIdentityAllowed?: (reference: string, context?: ToolContext) => boolean;
  vaultAllowed?: (context?: ToolContext) => boolean;
  subject?: string;
  sshPassword?: (reference: string) => Promise<string>;
}>): Promise<ManagedComputerRuntime> {
  let disposed = false;
  const lifetime = new AbortController();
  const calls = new AsyncLocalStorage<ToolContext>();
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    lifetime.abort(new Error("managed computer runtime is disposed"));
    options.computer[Symbol.dispose]();
  };

  try {
    const filesystem = options.filesystem ?? await createWorkspaceFilesystem(options.computer);
    const fetch = createManagedShellFetch(
      options.egress,
      options.subject,
      options.connectorAllowed === undefined ? undefined
        : (connector, connectionId) => options.connectorAllowed!(connector, connectionId, calls.getStore()),
      () => options.vaultAllowed?.(calls.getStore()) ?? true,
    );
    const runtime = await createComputerRuntime({
      filesystem,
      refreshFilesystemBeforeExec: options.filesystem !== undefined,
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
            : { sshIdentityAllowed: (reference: string) => options.sshIdentityAllowed!(reference, calls.getStore()) }),
          ...(options.subject === undefined ? {} : { subject: options.subject }),
        }),
      }],
    });
    return Object.freeze({
      ...runtime,
      dispose,
      tool: Object.freeze({
        ...runtime.tool, dispose,
        handler: (input: unknown, context: ToolContext) => {
          if (disposed) throw new Error("managed computer runtime is disposed");
          const scoped = { ...context, signal: AbortSignal.any([context.signal, lifetime.signal]) };
          return calls.run(scoped, () => runtime.tool.handler(input, scoped));
        },
      }),
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
  vaultAllowed: () => boolean = () => true,
): ShellFetch {
  const stream: NonNullable<ShellFetch["stream"]> = async (url, options = {}) => {
    const method = (options.method ?? "GET").toUpperCase();
    const request = new Request(url, {
      method,
      headers: options.headers,
      ...(method === "GET" || method === "HEAD" || options.body === undefined
        ? {}
        : { body: options.body }),
      signal: options.signal,
    });
    if (request.headers.has(VAULT_ID_HEADER) && !vaultAllowed()) {
      throw new Error("the current authorization cannot use Vault items");
    }
    const response = await handleManagedEgress(request, binding, subject, connectorAllowed);
    const headers: Record<string, string> = Object.create(null) as Record<string, string>;
    response.headers.forEach((value, name) => { headers[name] = value; });
    return {
      status: response.status,
      statusText: response.statusText,
      headers,
      body: shellResponseChunks(response, options.signal),
      url: response.url || request.url,
    };
  };
  return Object.assign(async (url: string, options: Parameters<ShellFetch>[1] = {}) => {
    const response = await stream(url, options);
    const chunks: Uint8Array[] = [];
    let size = 0;
    for await (const chunk of response.body) { chunks.push(chunk); size += chunk.byteLength; }
    const body = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
    return { ...response, body };
  }, { stream });
}

async function* shellResponseChunks(response: Response, signal?: AbortSignal): AsyncIterable<Uint8Array> {
  if (response.body === null) { signal?.throwIfAborted(); return; }
  const reader = response.body.getReader();
  const abort = () => { void reader.cancel(signal?.reason).catch(() => {}); };
  signal?.addEventListener("abort", abort, { once: true });
  let complete = false;
  try {
    for (;;) {
      signal?.throwIfAborted();
      const { done, value } = await reader.read();
      if (done) { complete = true; break; }
      yield value;
    }
    signal?.throwIfAborted();
  } finally {
    if (!complete) {
      try { await reader.cancel(signal?.reason); } catch { /* Preserve the read failure. */ }
    }
    signal?.removeEventListener("abort", abort);
    reader.releaseLock();
  }
}
