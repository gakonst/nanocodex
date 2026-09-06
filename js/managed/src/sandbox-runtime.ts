import {
  ContainerProxy as CloudflareContainerProxy,
  Sandbox as CloudflareSandbox,
} from "@cloudflare/sandbox";

import { handleManagedEgress } from "./managed-egress";

export type SandboxRuntimeEnv = Readonly<{
  NANOCODEX: Fetcher;
}>;

/**
 * The account sandbox is untrusted execution. Public HTTP(S) is allowed only
 * after the managed egress policy has validated the request; every other
 * outbound protocol is denied by the container boundary.
 */
export class Sandbox extends CloudflareSandbox<SandboxRuntimeEnv> {
  override enableInternet = false;
  override interceptHttps = true;

  /** Called over trusted Worker RPC, never derived from container headers. */
  async bindAccountEgress(subject: string): Promise<void> {
    if (!/^[A-Za-z0-9_-]{43,128}$/.test(subject)) throw new Error("invalid sandbox account subject");
    await this.ctx.blockConcurrencyWhile(async () => {
      const current = await this.ctx.storage.get<string>("nanocodex-egress-subject");
      if (current !== undefined && current !== subject) throw new Error("sandbox belongs to another account subject");
      if (current === subject) return;
      await this.setOutboundHandler("account", { subject });
      await this.ctx.storage.put("nanocodex-egress-subject", subject);
    });
  }
}

export async function handleSandboxEgress(
  request: Request,
  env: SandboxRuntimeEnv,
  context?: Readonly<{ params?: unknown }>,
): Promise<Response> {
  const headers = new Headers(request.headers);
  // The transparent container proxy reconstructs this from the intercepted
  // destination. It is transport metadata, not a caller-controlled credential,
  // and Request will derive the upstream Host header from the validated URL.
  headers.delete("host");
  // gh requires a token to enter its HTTP path. It receives only this public
  // marker; the broker injects the actual credential for the bound account.
  const origin = new URL(request.url).origin;
  const authorization = headers.get("authorization") ?? "";
  if ((origin === "https://api.github.com" || origin === "https://github.com")
    && (/^(?:Bearer|token) NANOCODEX_PROVIDER_CREDENTIAL$/i.test(authorization)
      || authorization === `Basic ${btoa("x-access-token:NANOCODEX_PROVIDER_CREDENTIAL")}`)) {
    headers.delete("authorization");
  }
  const sanitized = new Request(request, { headers });
  const params = context?.params as { subject?: string } | undefined;
  return handleManagedEgress(sanitized, env.NANOCODEX, params?.subject);
}

Sandbox.outbound = handleSandboxEgress;
Sandbox.outboundHandlers = { account: handleSandboxEgress };

/**
 * Required by the Sandbox SDK for transparent HTTP(S) interception. Sandbox
 * 0.12.4 does not apply a source mount's prefix to cross-binding S3 COPY
 * requests. We bind one alias per peer prefix, so fail those server-side copies
 * closed; ordinary cross-mount filesystem copies stream reads and writes and do
 * not require this optimization.
 */
export class ContainerProxy extends CloudflareContainerProxy {
  override fetch(request: Request): Promise<Response> {
    if (isCrossBindingR2Copy(request)) {
      return Promise.resolve(new Response("Cross-binding R2 copy is forbidden", { status: 403 }));
    }
    return super.fetch(request);
  }
}

export function isCrossBindingR2Copy(request: Request): boolean {
  const url = new URL(request.url);
  const copySource = request.headers.get("x-amz-copy-source");
  if (url.hostname !== "r2.internal" || copySource === null) return false;
  try {
    const destinationBinding = bucketBinding(url.pathname);
    const sourcePath = decodeURIComponent(copySource.split("?", 1)[0] ?? "");
    const sourceBinding = bucketBinding(sourcePath);
    return destinationBinding === undefined
      || sourceBinding === undefined
      || sourceBinding !== destinationBinding;
  } catch {
    return true;
  }
}

function bucketBinding(path: string): string | undefined {
  const stripped = path.replace(/^\/+/, "");
  const binding = stripped.split("/", 1)[0];
  return binding || undefined;
}
