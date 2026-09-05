export type ChiefOfStaffProxyEnv = {
  CHIEF_OF_STAFF?: Readonly<{ fetch(request: Request): Promise<Response> }>;
};

const statusPath = "/api/chief-of-staff/status";
const installPath = "/api/chief-of-staff/slack/install";
const installationPath = /^\/api\/chief-of-staff\/slack\/installations\/(T[A-Z0-9]+)$/;

export async function routeChiefOfStaff(
  request: Request,
  env: ChiefOfStaffProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  const installation = url.pathname.match(installationPath);
  const target = url.pathname === statusPath
    ? { method: "GET", path: "/v1/readiness" }
    : url.pathname === installPath
      ? { method: "GET", path: "/v1/slack/install" }
      : installation
        ? { method: "DELETE", path: `/v1/slack/installations/${installation[1]}` }
        : undefined;
  if (!target) return undefined;
  if (request.method !== target.method) {
    return json({ error: "method_not_allowed" }, {
      status: 405,
      headers: { allow: target.method },
    });
  }
  if (!env.CHIEF_OF_STAFF) {
    return json({ error: "chief_of_staff_unavailable" }, { status: 503 });
  }
  try {
    return await env.CHIEF_OF_STAFF.fetch(new Request(
      new URL(target.path, "https://chief-of-staff.internal"),
      { headers: request.headers, method: target.method },
    ));
  } catch (error) {
    console.error({
      type: "chief_of_staff.backend_failure",
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "chief_of_staff_unavailable" }, { status: 503 });
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
