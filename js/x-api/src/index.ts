import { parseXRequest } from "nanocodex-tools/x";
import { browse, browseResponse } from "./browse.js";
import { convertTweet, markdownResponse } from "./converter.js";
import { ConvertError } from "./errors.js";

export default {
  async fetch(request: Request): Promise<Response> {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return Response.json({ error: { code: "method_not_allowed", message: "Use GET or HEAD." } },
        { status: 405, headers: { allow: "GET, HEAD", "cache-control": "no-store" } });
    }
    const signal = AbortSignal.any([request.signal, AbortSignal.timeout(25_000)]);
    try {
      const url = new URL(request.url);
      if (url.search.length > 8192) throw new ConvertError(400, "Query is too long.", "invalid_request");
      const params = url.searchParams;
      const duplicate = [...params.keys()].find((key) => params.getAll(key).length !== 1);
      if (duplicate) throw new ConvertError(400, `Duplicate parameter: ${duplicate}`, "invalid_request");
      const format = params.get("format") ?? (request.headers.get("accept")?.includes("application/json") ? "json" : "markdown");
      params.delete("format");
      let action: string;
      if (url.pathname === "/api/convert") {
        action = "post";
      } else if (url.pathname === "/api/browse") {
        action = params.get("resource") ?? "";
        params.delete("resource");
      } else {
        const status = /^\/([A-Za-z0-9_]{1,15})\/status\/(\d{1,25})\/?$/.exec(url.pathname);
        const profile = /^\/([A-Za-z0-9_]{1,15})(?:\/(followers|following))?\/?$/.exec(url.pathname);
        if (status) {
          action = "post";
          params.set("url", `https://x.com/${status[1]}/status/${status[2]}`);
        } else if (url.pathname === "/search") {
          action = "search";
        } else if (profile) {
          action = profile[2] ?? "profile";
          params.set("handle", profile[1]!);
        } else throw new ConvertError(404, "Unknown X API route.", "not_found");
      }
      if (!["markdown", "json", ...(action === "post" ? ["obsidian"] : [])].includes(format)) {
        throw new ConvertError(400, "Unsupported output format.", "invalid_format");
      }
      const input: Record<string, unknown> = { action };
      for (const [key, value] of params) {
        if (["full", "nocache"].includes(key)) {
          if (!["true", "false", "1", "0", "yes", "no"].includes(value)) throw new ConvertError(400, `Invalid X ${key}`, "invalid_request");
          input[key] = ["true", "1", "yes"].includes(value);
        } else input[key] = ["page", "limit"].includes(key) ? Number(value) : value;
      }
      // Route determines action; callers cannot override it in the query.
      if (params.has("action")) throw new ConvertError(400, "Unknown X request field: action", "invalid_request");
      let validated;
      try { validated = parseXRequest(input); }
      catch (error) {
        throw new ConvertError(400, error instanceof Error ? error.message : "Invalid X request", "invalid_request");
      }
      signal.throwIfAborted();
      const result = action === "post"
        ? markdownResponse(await convertTweet({ ...validated, format }, signal), format === "json")
        : browseResponse(await browse({ ...validated, resource: action, format }, signal), format === "json");
      signal.throwIfAborted();
      return new Response(request.method === "HEAD" ? null : result.body, {
        status: result.status,
        headers: { ...result.headers, "cache-control": "no-store", "x-content-type-options": "nosniff" },
      });
    } catch (error) {
      const failure = signal.aborted ? new ConvertError(504, "X request timed out or was cancelled.", "request_timeout", 30)
        : error instanceof ConvertError ? error
        : new ConvertError(502, "X data provider returned an invalid response.", "provider_error");
      const retryAfter = failure.retryAfter ?? (failure.status === 503 ? 30 : undefined);
      return new Response(request.method === "HEAD" ? null : JSON.stringify({
        error: { code: failure.code, message: failure.message },
        ...(retryAfter === undefined ? {} : { retry_after: retryAfter }),
      }), { status: failure.status, headers: {
        "content-type": "application/json", "cache-control": "no-store",
        ...(retryAfter === undefined ? {} : { "retry-after": String(retryAfter) }),
      } });
    }
  },
} satisfies ExportedHandler;
