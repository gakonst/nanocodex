import type { Kv } from "accounts/server";
import { connectorCapabilities } from "./connectorPolicy.mts";
import { authenticateMcp, boundedObject, OAuthFailure, oauthJson, type McpGrant, type McpOAuthHooks } from "./oauthMcp.mts";

type Tool = { name: string; description: string; inputSchema: Record<string, unknown>; annotations: Record<string, boolean> };
export type McpTools = {
  call(name: string, args: Record<string, unknown>, grant: McpGrant, request: Request): Promise<Response>;
  events?: {
    capabilities(grant: McpGrant): Record<string, unknown>;
    call(method: string, params: Record<string, unknown>, grant: McpGrant, request: Request): Promise<unknown>;
  };
};
const legacyVersions = ["2025-03-26", "2025-06-18", "2025-11-25"];
const modernVersion = "2026-07-28";
const versions = [...legacyVersions, modernVersion];
const metaPrefix = "io.modelcontextprotocol/";
const serverInfo = { name: "nanocodex", version: "1.0.0" };
// Only Mcp-Name supports the UTF-8 base64 sentinel. Decode before comparison,
// rejecting malformed encodings rather than accepting a lossy header value.
function headerName(value: string | null): string | null {
  if (value === null) return null;
  if (!value.startsWith("=?base64?") || !value.endsWith("?=")) return /^[\x20-\x7e\t]*$/.test(value) && value.trim() === value ? value : null;
  try {
    const encoded = value.slice(9, -2);
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) return null;
    const decoded = atob(encoded);
    if (btoa(decoded) !== encoded) return null;
    return new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(decoded, char => char.charCodeAt(0)));
  } catch { return null; }
}
const dataRead = ["document_get", "document_list", "timeseries_list", "timeseries_query", "timeseries_aggregate", "object_get", "object_list"];
const dataWrite = ["document_put", "document_delete", "timeseries_write", "object_put", "object_delete"];
const memoryRead = ["list", "read", "search", "status"];
const memoryWrite = ["add_ad_hoc_note", "write"];
function object(properties: Record<string, unknown>, required: string[] = [], additionalProperties = false) {
  return { type: "object", properties, required, additionalProperties };
}
const string = { type: "string" };
function tool(name: string, description: string, inputSchema: Record<string, unknown>, readOnly = true): Tool {
  return { name, description, inputSchema, annotations: { readOnlyHint: readOnly, destructiveHint: !readOnly, idempotentHint: readOnly, openWorldHint: true } };
}
export function mcpTools(grant: McpGrant): Tool[] {
  const scopes = new Set(grant.scope?.split(" ") ?? []), capabilities = new Set(grant.capabilities);
  const result = [tool("nanocodex_connection", "Read the current scoped Nanocodex connection and approved capabilities. Does not expose tokens.", object({}))];
  if (scopes.has("agent:run") && capabilities.has("chatgpt")) result.push(
    tool("nanocodex_agent_start", "Start a Nanocodex agent turn. Retain one UUID operation_id per intended prompt and reuse identical arguments after uncertainty. Returns an acceptance receipt; poll nanocodex_agent_status for completion.", object({ prompt: { type: "string", minLength: 1, maxLength: 32000 }, operation_id: { type: "string", format: "uuid" } }, ["prompt", "operation_id"]), false),
    tool("nanocodex_agent_status", "Read the approved agent or one turn's status and final output.", object({ turn_id: string })),
  );
  for (const mode of ["read", "write"] as const) {
    if (scopes.has(`data:${mode}`) && capabilities.has(`data:${mode}`)) result.push(tool(`nanocodex_data_${mode}`, `Perform a ${mode} operation on the approved Nanocodex user data store. Pass the operation and its documented fields; versioned writes should include if_version.`, object({ operation: { type: "string", enum: mode === "read" ? dataRead : dataWrite }, key: string, prefix: string, cursor: string, limit: { type: "integer" }, series: string, if_version: { type: "integer", minimum: 0 }, value: {}, content: string, content_type: string,
      encoding: { type: "string", enum: ["utf8", "base64"] }, metadata: {}, sha256: string, points: { type: "array", items: { type: "object" } },
      start_ms: { type: "integer" }, end_ms: { type: "integer" }, bucket_ms: { type: "integer" }, order: { type: "string", enum: ["asc", "desc"] },
      aggregation: { type: "string", enum: ["avg", "min", "max", "sum", "count"] }, conflict: { type: "string", enum: ["error", "replace"] } }, ["operation"], true), mode === "read"));
    if (scopes.has(`memory:${mode}`) && capabilities.has(`memory:${mode}`)) result.push(tool(`nanocodex_memory_${mode}`, `Perform a ${mode} operation on this Connect grant's authorized shared team memory. Private account memory is not included.`, object({ operation: { type: "string", enum: mode === "read" ? memoryRead : memoryWrite }, path: string, queries: { type: "array", items: string }, content: string, write_operation: { type: "string", enum: ["put", "append", "delete"] },
      filename: string, note: string, line_offset: { type: "integer" }, max_lines: { type: "integer" }, max_results: { type: "integer" },
      cursor: string, case_sensitive: { type: "boolean" }, normalized: { type: "boolean" }, context_lines: { type: "integer" }, match_mode: {},
      scope: { type: "string", enum: ["team"] }, user_requested: { type: "boolean" } }, ["operation"], true), mode === "read"));
  }
  if (scopes.has("history:read") && capabilities.has("history:read")) result.push(
    tool("nanocodex_history_search", "Search completed sessions visible to the authorized team. Read matching turns before relying on them.", object({ query: string, limit: { type: "integer", minimum: 1, maximum: 100 } }, ["query"])),
    tool("nanocodex_history_read", "Read completed turns from a session visible to the authorized team.", object({ session_id: string, turn_ids: { type: "array", items: string } }, ["session_id"])),
  );
  for (const connector of connectorCapabilities) {
    if (connector !== "chatgpt" && scopes.has(`connector:${connector}`) && capabilities.has(connector)) result.push(tool(`nanocodex_${connector}_request`, `Call the approved ${connector} API with the user's selected connection. Authentication stays in Nanocodex. Use an exact connection_id from nanocodex_connection when multiple identities are selected. Only user-authorized writes are permitted.`, object({ path: { type: "string", minLength: 1 }, method: { type: "string", enum: ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"] }, connection_id: string, headers: { type: "object", additionalProperties: string }, body: {} }, ["path"]), false));
  }
  return result;
}
function isRecord(value: unknown): value is Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value); }
function validArguments(name: string, args: Record<string, unknown>, schema: Record<string, unknown>): boolean {
  const properties = schema.properties as Record<string, Record<string, unknown>>;
  if ((schema.required as string[]).some(key => !(key in args))) return false;
  if (schema.additionalProperties === false && Object.keys(args).some(key => !(key in properties))) return false;
  for (const [key, value] of Object.entries(args)) {
    const property = properties[key]; if (!property) continue;
    if (property.type === "string" && (typeof value !== "string" || (typeof property.minLength === "number" && value.length < property.minLength) || (typeof property.maxLength === "number" && value.length > property.maxLength))) return false;
    if (property.type === "integer" && (!Number.isSafeInteger(value) || (typeof property.minimum === "number" && Number(value) < property.minimum) || (typeof property.maximum === "number" && Number(value) > property.maximum))) return false;
    if (Array.isArray(property.enum) && !property.enum.includes(value)) return false;
    if (property.type === "array" && (!Array.isArray(value) || ((property.items as Record<string, unknown> | undefined)?.type === "string" && value.some(v => typeof v !== "string")))) return false;
    if (property.type === "boolean" && typeof value !== "boolean") return false;
    if (property.type === "object" && (!isRecord(value) || Object.values(value).some(v => typeof v !== "string"))) return false;
  }
  if (name === "nanocodex_memory_write" && args.operation === "write" && !["put", "append", "delete"].includes(String(args.write_operation))) return false;
  if (name === "nanocodex_agent_start" && !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(String(args.operation_id))) return false;
  if (name === "nanocodex_agent_status" && args.turn_id !== undefined && !/^[A-Za-z0-9_-]{1,128}$/.test(String(args.turn_id))) return false;
  return true;
}
export async function mcpServer(request: Request, store: Kv.Kv, hooks: McpOAuthHooks, tools: McpTools): Promise<Response> {
  const metadata = new URL("/.well-known/oauth-protected-resource/mcp", request.url).href;
  let grant: McpGrant | undefined;
  try { grant = await authenticateMcp(request, store, hooks); } catch (error) {
    if (error instanceof OAuthFailure) return oauthJson({ error: error.error, error_description: error.message }, error.status);
    throw error;
  }
  if (!grant) return oauthJson({ error: "invalid_token", error_description: "Authorize this MCP resource with Nanocodex Connect." }, 401,
    { "www-authenticate": `Bearer resource_metadata="${metadata}", error="invalid_token"` });
  if (request.method !== "POST") return oauthJson({ error: "method_not_allowed" }, 405, { allow: "POST" });
  const version = request.headers.get("mcp-protocol-version");
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) return oauthJson({ error: "unsupported_media_type" }, 415);
  const accepts = request.headers.get("accept") ?? "";
  if (!accepts.includes("application/json") || !accepts.includes("text/event-stream")) return oauthJson({ error: "not_acceptable", error_description: "Accept application/json and text/event-stream." }, 406);
  let message: Record<string, unknown>;
  const rpcError = (id: unknown, code: number, message: string, status = 200, data?: unknown) => oauthJson({ jsonrpc: "2.0", ...(id === null && version === modernVersion ? {} : { id }), error: { code, message, ...(data === undefined ? {} : { data }) } }, status);
  try { message = await boundedObject(request); } catch (error) {
    return rpcError(null, error instanceof OAuthFailure && error.error === "invalid_json" ? -32700 : -32600, "Invalid JSON-RPC request.", error instanceof OAuthFailure && error.status === 413 ? 413 : 400);
  }
  const id = message.id;
  if (message.jsonrpc !== "2.0" || typeof message.method !== "string" || (id !== undefined && typeof id !== "string" && !(typeof id === "number" && Number.isFinite(id)))
    || (message.params !== undefined && !isRecord(message.params))) return rpcError(null, -32600, "Invalid JSON-RPC request.", 400);
  if (id === undefined) {
    // JSON-RPC notifications never execute tools here and never receive a
    // JSON-RPC response. This server has no pending server-to-client requests.
    return new Response(null, { status: 202, headers: { "cache-control": "no-store" } });
  }
  const params = (message.params ?? {}) as Record<string, unknown>;
  const meta = isRecord(params._meta) ? params._meta : {};
  const bodyVersion = meta[`${metaPrefix}protocolVersion`];
  const modern = version === modernVersion || bodyVersion !== undefined || message.method === "server/discover";
  if (modern) {
    if (version === null || typeof bodyVersion !== "string" || version !== bodyVersion
      || request.headers.get("mcp-method") !== message.method
      || (["tools/call", "prompts/get", "resources/read"].includes(message.method)
        && (headerName(request.headers.get("mcp-name")) === null || headerName(request.headers.get("mcp-name")) !== (message.method === "resources/read" ? params.uri : params.name)))) {
      return rpcError(id, -32020, "Required MCP headers are missing or do not match the request body.", 400);
    }
    if (!versions.includes(version)) return rpcError(id, -32022, "Unsupported protocol version.", 400, { supported: versions, requested: version });
    if (!isRecord(meta[`${metaPrefix}clientCapabilities`])) return rpcError(id, -32602, "Per-request clientCapabilities must be an object.");
    const info = meta[`${metaPrefix}clientInfo`];
    if (info !== undefined && (!isRecord(info) || typeof info.name !== "string" || typeof info.version !== "string")) return rpcError(id, -32602, "Invalid clientInfo.");
  } else if (version !== null && !versions.includes(version)) {
    return rpcError(id, -32022, "Unsupported protocol version.", 400, { supported: versions, requested: version });
  }
  const success = (result: unknown) => oauthJson({ jsonrpc: "2.0", id, result: modern
    ? { ...(isRecord(result) ? result : {}), resultType: "complete", _meta: { ...(isRecord(result) && isRecord(result._meta) ? result._meta : {}), [`${metaPrefix}serverInfo`]: serverInfo } }
    : result });
  const capabilities = { tools: { listChanged: false }, ...(tools.events?.capabilities(grant) ?? {}) };
  if (message.method === "server/discover") return success({ supportedVersions: versions, capabilities, ttlMs: 0, cacheScope: "private" });
  if (message.method.startsWith("events/") && tools.events) {
    try { return success(await tools.events.call(message.method, params, grant, request)); }
    catch (error) {
      if (error !== null && typeof error === "object" && "code" in error && typeof error.code === "number" && "message" in error && typeof error.message === "string") {
        return rpcError(id, error.code, error.message, modern && error.code === -32601 ? 404 : 200, "data" in error ? error.data : undefined);
      }
      throw error;
    }
  }
  if (message.method === "initialize" && !modern) {
    if (typeof params.protocolVersion !== "string" || !isRecord(params.capabilities) || !isRecord(params.clientInfo) || typeof params.clientInfo.name !== "string" || typeof params.clientInfo.version !== "string") return rpcError(id, -32602, "initialize requires protocolVersion, capabilities, and clientInfo.");
    return success({ protocolVersion: legacyVersions.includes(params.protocolVersion) ? params.protocolVersion : legacyVersions.at(-1), capabilities, serverInfo, instructions: "Tools use only the permissions approved in Nanocodex Connect. Mutations require the user's authorization. Retain operation IDs after an uncertain agent start." });
  }
  if (message.method === "ping") return success({});
  if (message.method === "tools/list") {
    if (params.cursor !== undefined) return rpcError(id, -32602, "This tool catalog has no additional pages.");
    return success({ tools: mcpTools(grant), ...(modern ? { ttlMs: 0, cacheScope: "private" } : {}) });
  }
  if (message.method === "tools/call") {
    const selected = mcpTools(grant).find(tool => tool.name === params.name);
    if (!selected) return rpcError(id, -32602, "Unknown or unapproved tool.");
    const args = params.arguments ?? {};
    if (!isRecord(args) || !validArguments(selected.name, args, selected.inputSchema)) return rpcError(id, -32602, "Invalid tool arguments.");
    // Resolve again immediately before dispatch; token/grant expiry and refresh
    // family revocation are authorization decisions, never cached catalog state.
    const current = await authenticateMcp(request, store, hooks);
    if (!current) return oauthJson({ error: "invalid_token" }, 401, { "www-authenticate": `Bearer resource_metadata="${metadata}", error="invalid_token"` });
    try {
      const response = await tools.call(selected.name, args, current, request);
      const text = await response.text();
      let structured: unknown; try { structured = JSON.parse(text); } catch { /* plain text response */ }
      return success({ content: [{ type: "text", text }], ...(isRecord(structured) ? { structuredContent: structured } : {}), ...(!response.ok ? { isError: true } : {}) });
    } catch (error) {
      // Connector/account errors are tool results, protocol errors are above.
      return success({ content: [{ type: "text", text: error instanceof Error ? error.message : "The tool failed." }], isError: true });
    }
  }
  return rpcError(id, -32601, "Method not found.", modern ? 404 : 200);
}
