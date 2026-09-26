import { web } from "nanocodex/tools";
import { tracing } from "cloudflare:workers";
import type { NamedTool, ToolContext } from "nanocodex";

const SEARCH_URL = "https://nanocodex.internal/v1/search";
export type WebTimingObserver = (context: ToolContext, phase: string, durationMs: number) => void;

/** Standard web.run schema/decoder, routed through the private, credential-free Egress2 binding. */
export function managedWeb({ egress, owner, onTiming, correlation, clock = () => performance.now() }: {
  egress: Fetcher; owner: string; onTiming?: WebTimingObserver; correlation?: (context: ToolContext) => string | undefined; clock?: () => number;
}): NamedTool {
  const standard = web({ url: SEARCH_URL, fetch: async () => { throw new Error("web adapter not initialized"); } });
  return { ...standard, async handler(input, context) {
    // Tool definitions are shared across turns. All timing/response state belongs to this invocation.
    const started = clock();
    let preparedAt: number | undefined, headersAt: number | undefined;
    let serverTiming = "";
    const observe = (phase: string, durationMs: number) => {
      try { onTiming?.(context, phase, Math.max(0, durationMs)); }
      catch { /* telemetry cannot break a tool call */ }
    };
    const tool = web({ url: SEARCH_URL, fetch: async (_url, init) => {
      preparedAt = clock();
      observe("preparation", preparedAt - started);
      try {
        const response = await tracing.enterSpan("managed2.web.egress", async span => {
          const traceId = correlation?.(context);
          if (traceId) span.setAttribute("managed2.trace_id", traceId);
          const response = await egress.fetch(new Request(SEARCH_URL, {
          method: "POST", redirect: "manual", signal: init?.signal,
          headers: { "x-managed2-owner": owner,
            ...(traceId ? { "x-managed2-trace-id": traceId } : {}),
            authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" },
          body: init?.body,
        }));
          span.setAttribute("http.response.status_code", response.status);
          return response;
        });
        serverTiming = response.headers.get("server-timing") ?? "";
        return response;
      } finally {
        headersAt = clock();
        observe("egress_dispatch", headersAt - preparedAt);
      }
    } });
    try { return await tracing.enterSpan("managed2.web.parse", () => tool.handler(input, context)); }
    finally {
      const ended = clock();
      if (preparedAt === undefined) observe("preparation", ended - started);
      if (headersAt !== undefined) observe("parse", ended - headersAt);
      // These server-side timings come from our own private Egress2 service. Treat missing
      // or malformed values as absent; never emit upstream-provided timing labels.
      for (const [label, phase] of [
        ["search_prepare", "egress_prepare"], ["search_credential", "egress_credential"],
        ["search_upstream", "egress_upstream"], ["search_parse", "egress_parse"],
      ] as const) {
        const match = new RegExp(`(?:^|,\\s*)${label};dur=([0-9]+(?:\\.[0-9]+)?)(?:,|$)`).exec(serverTiming);
        if (match) {
          const value = Number(match[1]);
          if (Number.isFinite(value)) observe(phase, value);
        }
      }
    }
  } };
}
