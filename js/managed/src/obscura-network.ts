import { WorkerEntrypoint } from "cloudflare:workers";
import { handleManagedEgress } from "./managed-egress";

interface ObscuraNetworkEnv {
  NANOCODEX: Fetcher;
}

/** Public page network only: no account subject, Vault routing, or connectors. */
export class ObscuraNetwork extends WorkerEntrypoint<ObscuraNetworkEnv> {
  async fetch(request: Request): Promise<Response> {
    // Reject before the existing helper's early Vault dispatch.
    for (const name of request.headers.keys()) {
      if (
        name.toLowerCase().startsWith("x-nanocodex-") ||
        /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i.test(
          name,
        )
      ) {
        return Response.json(
          { error: "credential_header_denied" },
          { status: 403 },
        );
      }
    }
    // Redirects return to the engine; each destination must re-enter this policy.
    return handleManagedEgress(
      request,
      this.env.NANOCODEX,
      undefined,
      () => false,
    );
  }
}
