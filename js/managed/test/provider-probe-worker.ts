import { ProviderProbeCoordinator as Coordinator } from "../src/provider-probe-coordinator";

/** A real workerd/SQLite coordinator with a synthetic Workers AI binding only. */
export class ProviderProbeCoordinator extends Coordinator {
  constructor(ctx: DurableObjectState, env: object) {
    super(ctx, { ...env, NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT: "3",
      AI: { async run() {
        const data = [{ choices: [{ index: 0, delta: { content: "OK" }, finish_reason: null }] },
          { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }];
        return new Response(data.map(value => `data: ${JSON.stringify(value)}\n\n`).join("") + "data: [DONE]\n\n").body;
      } },
    });
  }
}
