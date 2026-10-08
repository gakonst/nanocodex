import { AccountHostedTools } from "./account-hosted-tools";
import type { RegionalHandEnv } from "./regional-hand-routing";
import type { RemoteICEEnv } from "./hand-remote-ice";

/** One bounded regional shard per account: tool-host publishers and native
 * screen signaling (rs.<region>. IDs). Authority and VM/server publishers stay on the owner. */
export class RegionalHandRelay extends AccountHostedTools {
  constructor(ctx: DurableObjectState, env: RemoteICEEnv & RegionalHandEnv) {
    super(ctx, env, true);
  }
}
export { routeRegionalToolHost } from "./regional-hand-routing";
