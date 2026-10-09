import type { NamedTool, ToolContext } from "nanocodex";
import { SITE_ID } from "@nanocodex/sites/format";
import type { Principal } from "./account-auth";

const SHARE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const PUBLISHED_SITE_SCHEMA = {
  type: "object",
  additionalProperties: false,
  required: ["type", "site_id", "title", "version", "entry", "files", "bytes", "excluded", "created"],
  properties: {
    type: { type: "string", const: "nanocodex.site" },
    site_id: { type: "string" },
    title: { type: "string" },
    version: { type: "integer" },
    entry: { type: "string" },
    files: { type: "integer" },
    bytes: { type: "integer" },
    excluded: { type: "integer", description: "Files skipped because they look like secrets or dependencies." },
    created: { type: "boolean", description: "False when identical content was already the latest version." },
  },
};

type SiteToolOptions = {
  sessionId: string;
  ownerId: string;
  authorizationEpoch: number;
  origin: string;
  authorization(context: ToolContext): Principal | undefined;
  request(request: Request, principal: Principal): Promise<Response>;
};

/** Dispatch through the owner-authenticated public router, never model-supplied identity headers. */
export function siteTools(options: SiteToolOptions): NamedTool[] {
  const call = async (context: ToolContext, path: string, method: string, body?: unknown): Promise<Response> => {
    context.signal.throwIfAborted();
    const principal = options.authorization(context);
    if (context.subagent !== undefined || !principal
      || (principal.kind !== "account_session" && principal.kind !== "api_key")
      || principal.connectGrant !== undefined || principal.userId !== options.ownerId
      || principal.authorizationEpoch !== options.authorizationEpoch
      || !principal.capabilities.includes("agents:read") || !principal.capabilities.includes("tools:use")
      || (method !== "GET" && !principal.capabilities.includes("agents:write"))) {
      throw new Error("Sites require current direct account root authorization with agents:read, agents:write, and tools:use");
    }
    const url = new URL(`/v1/agents/${options.sessionId}/sites${path}`, options.origin);
    return options.request(new Request(url, {
      method,
      headers: { origin: url.origin, ...(body === undefined ? {} : { "content-type": "application/json" }) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      signal: context.signal,
    }), principal);
  };
  const failure = async (response: Response, action: string): Promise<never> => {
    let message = `HTTP ${response.status}`;
    try { message = (await response.json<{ message?: string }>()).message ?? message; } catch { /* keep the status */ }
    throw new Error(`Site ${action} failed: ${message}`);
  };

  return [{
    name: "publish_site",
    description: "Publish a static site version from this thread's files: a directory such as a build output containing index.html, or a single file such as a page, image, or PDF. path is absolute: /workspace/..., a Cloudflare workspace root, or /brain/.... Files are copied once into an immutable version; later edits need another publish. Publishing is private and creates no link. The user sees the version in the thread and can share it. Create a public link only with site_sharing, and only when the user explicitly asks. .env files, keys, .git, and node_modules are skipped. Reuse id to add a version to the same site; publishing unchanged files returns the existing version. Static files only: there is no server code. Reference files with relative URLs (app.js, ./app.js, Vite base './'); root-absolute URLs such as /app.js may not resolve.",
    parameters: { type: "object", additionalProperties: false, required: ["path"], properties: {
      path: { type: "string", description: "Absolute directory or file path, such as /workspace/app/dist." },
      id: { type: "string", pattern: SITE_ID.source, description: "Stable site ID. Defaults to a slug of the title or path." },
      title: { type: "string", maxLength: 200 },
      entry: { type: "string", description: "Relative file served at /. Defaults to index.html, or the only file." },
      spa: { type: "boolean", description: "Serve entry for unknown extensionless paths, for client-side routing." },
    } },
    outputSchema: PUBLISHED_SITE_SCHEMA,
    handler: async (input: unknown, context: ToolContext) => {
      const body = exactInput(input, ["path", "id", "title", "entry", "spa"]);
      if (typeof body.path !== "string") throw new TypeError("path is required");
      const response = await call(context, "", "POST", body);
      if (!response.ok) return failure(response, "publish");
      return response.json();
    },
  }, {
    name: "site_sharing",
    description: "List, create, or revoke public links to sites published in this thread. Create only when the user explicitly asks to share: anyone with the URL can open that site version until it is revoked or expires, and the link isn't indexed by search engines. version defaults to the latest; a link always serves the version it was created for. list returns metadata without URLs. Never retry create after an uncertain result: list first. Direct account root agent only.",
    parameters: { type: "object", additionalProperties: false, required: ["operation"], properties: {
      operation: { type: "string", enum: ["list", "create", "revoke"] },
      site_id: { type: "string", pattern: SITE_ID.source, description: "Required for create and revoke. Omit with list to list every site." },
      version: { type: "integer", minimum: 1, description: "Create only. Defaults to the latest version." },
      share_id: { type: "string", pattern: SHARE_ID.source, description: "Revoke only. Use an ID returned by list." },
    } },
    handler: async (input: unknown, context: ToolContext) => {
      const body = exactInput(input, ["operation", "site_id", "version", "share_id"]);
      if (body.operation === "list") {
        if (body.version !== undefined || body.share_id !== undefined) throw new TypeError("list accepts only site_id");
        const response = await call(context, "", "GET");
        if (!response.ok) return failure(response, "list");
        const { data } = await response.json<{ data: { id: string; title: string; latest_version: number; shares: { id: string; version: number; created_at: number; expires_at: number | null }[] }[] }>();
        return { sites: data.filter(site => body.site_id === undefined || site.id === body.site_id).map(site => ({
          site_id: site.id, title: site.title, latest_version: site.latest_version,
          shares: site.shares.map(({ id, version, created_at, expires_at }) => ({ share_id: id, version, created_at, expires_at })),
        })) };
      }
      if (typeof body.site_id !== "string" || !SITE_ID.test(body.site_id)) throw new TypeError("site_id is required");
      if (body.operation === "create") {
        if (body.share_id !== undefined) throw new TypeError("create does not accept share_id");
        let response: Response;
        try {
          response = await call(context, `/${body.site_id}/shares`, "POST", body.version === undefined ? {} : { version: body.version });
        } catch (error) {
          throw new Error("Site link creation outcome is unknown; do not retry automatically. List links first.", { cause: error });
        }
        if (!response.ok) {
          if (response.status >= 500) throw new Error(`Site link creation outcome is unknown (HTTP ${response.status}); list links before retrying.`);
          return failure(response, "link creation");
        }
        return response.json();
      }
      if (body.operation === "revoke") {
        if (typeof body.share_id !== "string" || !SHARE_ID.test(body.share_id) || body.version !== undefined) throw new TypeError("revoke requires share_id");
        const response = await call(context, `/${body.site_id}/shares/${body.share_id}`, "DELETE");
        if (!response.ok) return failure(response, "revoke");
        return { site_id: body.site_id, share_id: body.share_id, revoked: true };
      }
      throw new TypeError("Invalid site sharing operation");
    },
  }];
}

function exactInput(input: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("Expected an object");
  if (Object.keys(input).some(key => !keys.includes(key))) throw new TypeError(`Unexpected argument; expected ${keys.join(", ")}`);
  return input as Record<string, unknown>;
}
