import {
  ContainerProxy as CloudflareContainerProxy,
  Sandbox as CloudflareSandbox,
} from "@cloudflare/sandbox";

import { handleManagedEgress } from "./managed-egress";
import { BRAIN_INLINE_BYTES } from "./brain-bucket";

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
    const url = new URL(request.url);
    if (url.hostname === "r2.internal" && bucketBinding(url.pathname) === "NANOCODEX_BRAIN") {
      const props = this.ctx.props as { outboundByHostOverrides?: Record<string, {
        method: string; params?: { buckets?: Record<string, { prefix?: string; readOnly?: boolean }> };
      }> };
      const override = props.outboundByHostOverrides?.["r2.internal"];
      const mount = override?.method === "r2EgressMount" ? override.params?.buckets?.NANOCODEX_BRAIN : undefined;
      const match = /^\/?brains\/([A-Za-z0-9._:-]{1,256})\/?$/.exec(mount?.prefix ?? "");
      if (!match) return Promise.resolve(new Response("Brain mount is not authorized", { status: 403 }));
      const sessions = (this.env as { NANOCODEX_SESSIONS?: {
        getByName(name: string): { brainFilesystem(request: Request, readOnly: boolean): Promise<Response> };
      } }).NANOCODEX_SESSIONS;
      if (!sessions) return Promise.resolve(new Response("Brain storage binding is unavailable", { status: 503 }));
      // The actor id comes only from the SDK's trusted mount configuration.
      return sessions.getByName(match[1]!).brainFilesystem(request, mount?.readOnly === true);
    }
    return super.fetch(request);
  }
}

/** Reuse the SDK's S3 protocol, prefix and read-only checks over actor-local storage. */
export function serveBrainFilesystem(request: Request, bucket: R2Bucket, resourceId: string, readOnly: boolean): Promise<Response> {
  if (isCrossBindingR2Copy(request)) return Promise.resolve(new Response("Cross-binding R2 copy is forbidden", { status: 403 }));
  const context = { props: { outboundByHostOverrides: {
    "r2.internal": { method: "r2EgressMount", params: {
      buckets: { NANOCODEX_BRAIN: { prefix: `brains/${resourceId}`, readOnly } },
    } },
  } } } as unknown as ExecutionContext;
  const length = Number(request.headers.get("Content-Length") ?? NaN);
  const inline = request.method === "PUT" && !request.headers.has("x-amz-copy-source")
    && !new URL(request.url).searchParams.has("uploadId")
    && Number.isSafeInteger(length) && length >= 0 && length <= BRAIN_INLINE_BYTES;
  const selected = inline ? {
    ...bucket,
    async put(key: string, value: ReadableStream | ArrayBuffer | ArrayBufferView | string | Blob | null, options?: R2PutOptions) {
      // The SDK validates Content-Length and bounds this stream with
      // FixedLengthStream before invoking put. Larger bodies stay streaming.
      return bucket.put(key, value instanceof ReadableStream ? await new Response(value).arrayBuffer() : value, options);
    },
  } as R2Bucket : bucket;
  return new CloudflareContainerProxy(context, { NANOCODEX_BRAIN: selected }).fetch(request);
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
