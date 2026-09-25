import { web } from "nanocodex/tools";
import type { NamedTool, ToolContext } from "nanocodex";

const SEARCH_URL = "https://nanocodex.internal/v1/search";
export type WebTimingObserver = (context: ToolContext, phase: string, durationMs: number) => void;

/** Standard web.run schema/decoder, routed through the private, credential-free Egress2 binding. */
export function managedWeb({ egress, owner, onTiming, clock = () => performance.now() }: {
  egress: Fetcher; owner: string; onTiming?: WebTimingObserver; clock?: () => number;
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
        const response = await egress.fetch(new Request(SEARCH_URL, {
          method: "POST", redirect: "manual", signal: init?.signal,
          headers: { "x-managed2-owner": owner,
            authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" },
          body: init?.body,
        }));
        serverTiming = response.headers.get("server-timing") ?? "";
        return response;
      } finally {
        headersAt = clock();
        observe("egress_dispatch", headersAt - preparedAt);
      }
    } });
    try { return await tool.handler(input, context); }
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
