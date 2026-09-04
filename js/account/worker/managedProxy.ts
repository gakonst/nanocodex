export type ManagedProxyEnv = {
  NANOCODEX_BACKEND?: Fetcher;
};

const MANAGED_ROUTE = /^(?:\/auth(?:\/.*)?|\/webauthn\/.*|\/sandbox-preview\/[^/]+(?:\/.*)?|\/v1\/(?:auth(?:\/.*)?|me|model-capabilities|account\/(?:tool-host|vm-host)|system\/vm-host|vm-host-attachments\/[A-Za-z0-9_-]{43}\/[0-9a-f-]{36}\/tool-host|wallet(?:\/(?:balance|connect|revoke-access-key))?|egress|api-keys(?:\/.*)?|credentials(?:\/.*)?|connect(?:\/.*)?|connectors(?:\/.*)?|agents(?:\/.*)?|rooms(?:\/.*)?|history(?:\/.*)?|memory(?:\/.*)?|organization(?:\/.*)?))$/;

export function isManagedRoutePath(pathname: string): boolean {
  return MANAGED_ROUTE.test(pathname);
}

/**
 * Projects the private managed service onto the website origin.
 *
 * The managed service owns authentication, validation, account authorization,
 * room membership, and WebSocket upgrades. Keeping the original Request
 * preserves the browser's real origin and scoped cookies without a second
 * forwarding protocol.
 */
export async function routeManaged(
  request: Request,
  env: ManagedProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (!isManagedRoutePath(url.pathname)) return undefined;
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  try {
    return await env.NANOCODEX_BACKEND.fetch(request);
  } catch (error) {
    console.error({
      type: "managed.backend_failure",
      path: url.pathname,
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
}

function json(body: unknown, init: ResponseInit): Response {
  return Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init.headers,
    },
  });
}
