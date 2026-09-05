export type SshStream = Readonly<{
  closed(listener: (event: { error?: Error }) => void): Readonly<{ dispose(): void }>;
  close(error?: Error): Promise<void>;
  dispose(): void;
  readonly isDisposed: boolean;
  read(count: number, cancellation?: unknown): Promise<Uint8Array | null>;
  write(data: Uint8Array, cancellation?: unknown): Promise<void>;
}>;

export type SshEndpoint = URL | Readonly<{ hostname: string; port: number }>;
export type SshCommandResult = Readonly<{ stdout: string; stderr: string; exitCode: number }>;
export type SshIdentityReferenceRequest = Readonly<{
  identityReference: string;
  endpoint: SshEndpoint;
  username: string;
  commandArgs: readonly string[];
}>;

export type SshCommandOptions = Readonly<{
  transport: "tcp" | "websocket";
  maxOutputBytes?: number;
  openStream(endpoint: SshEndpoint, signal?: AbortSignal): Promise<SshStream>;
  readIdentity?(
    path: string,
    context: Readonly<{ cwd: string; signal?: AbortSignal }>,
  ): Promise<string>;
  resolvePassword?(reference: string): Promise<string>;
  executeWithIdentityReference?(
    request: SshIdentityReferenceRequest,
    context: Readonly<{ signal?: AbortSignal }>,
  ): Promise<SshCommandResult>;
}>;

/** Creates a non-interactive SSH command over a host-owned byte stream. */
export function createSshCommand(options: SshCommandOptions): Readonly<{
  name: "ssh";
  trusted?: boolean;
  execute(args: string[], context: unknown): Promise<SshCommandResult>;
}>;

/** Adapts a WHATWG byte-stream socket, including Cloudflare TCP sockets, to SSH. */
export function createWebStreamSshStream(
  socket: Readonly<{
    readable: ReadableStream<Uint8Array>;
    writable: WritableStream<Uint8Array>;
    closed?: Promise<void>;
    close(): Promise<void>;
  }>,
  signal?: AbortSignal,
): SshStream;
