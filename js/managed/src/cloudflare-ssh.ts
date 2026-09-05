import { connect as cloudflareConnect } from "cloudflare:sockets";
import {
  createWorkspaceSshCommand,
  type SshConnect,
  type Workspace,
} from "nanocodex-tools";
import type {
  SshCommandResult,
  SshIdentityReferenceRequest,
} from "nanocodex-tools/ssh";

/** Mounts direct and private-egress SSH into Just Bash without a Linux sandbox. */
export function createCloudflareSshCommand(options: Readonly<{
  connect?: SshConnect;
  egress?: Fetcher;
  filesystem(): Workspace;
  resolvePassword?(reference: string): Promise<string>;
  sshIdentityAllowed?(reference: string): boolean;
  subject?: string;
}>) {
  return createWorkspaceSshCommand({
    connect: options.connect ?? cloudflareConnect as SshConnect,
    filesystem: options.filesystem,
    ...(options.resolvePassword === undefined
      ? {}
      : { resolvePassword: options.resolvePassword }),
    ...(options.egress === undefined || options.subject === undefined
      ? {}
      : {
          executeWithIdentityReference: (
            request: SshIdentityReferenceRequest,
            context: Readonly<{ signal?: AbortSignal }>,
          ) => {
            if (options.sshIdentityAllowed !== undefined
              && !options.sshIdentityAllowed(request.identityReference)) {
              throw new Error("SSH identity is not granted for this turn");
            }
            return executeBrokeredSsh(options.egress!, options.subject!, request, context.signal);
          },
        }),
  });
}

async function executeBrokeredSsh(
  binding: Fetcher,
  subject: string,
  request: SshIdentityReferenceRequest,
  signal?: AbortSignal,
): Promise<SshCommandResult> {
  if (request.endpoint instanceof URL) throw new Error("brokered SSH requires a TCP endpoint");
  const response = await binding.fetch("https://ssh.internal/v1/execute", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-nanocodex-subject": subject,
    },
    body: JSON.stringify({
      identity_ref: request.identityReference,
      hostname: request.endpoint.hostname,
      port: request.endpoint.port,
      username: request.username,
      command: request.commandArgs,
    }),
    signal,
  });
  let body: unknown;
  try { body = await response.json(); } catch { throw new Error("private egress returned an invalid SSH response"); }
  if (!response.ok) {
    const code = stringField(body, "error");
    throw new Error(code ? `private egress rejected SSH (${code})` : "private egress rejected SSH");
  }
  const stdout = stringField(body, "stdout");
  const stderr = stringField(body, "stderr");
  const exitCode = numberField(body, "exit_code");
  if (stdout === undefined || stderr === undefined || exitCode === undefined) {
    throw new Error("private egress returned an invalid SSH result");
  }
  return { stdout, stderr, exitCode };
}

function stringField(value: unknown, field: string): string | undefined {
  return isRecord(value) && typeof value[field] === "string" ? value[field] : undefined;
}

function numberField(value: unknown, field: string): number | undefined {
  return isRecord(value) && Number.isSafeInteger(value[field]) ? value[field] as number : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
