import { setup } from "@rivet-dev/agentos";

import { nanocodexAuth } from "./auth.js";
import { nanocodex } from "./actors.js";

export const registry = setup({
  // AgentOS's setup enables the direct actor SQLite runtime socket and raises
  // transport limits for filesystem operations. Nanocodex still enforces its
  // own 1 MiB prompt boundary before accepting model work.
  // The embedding server owns its HTTP listener and process lifecycle, so it
  // coordinates both listeners and the child engine under one signal handler.
  shutdown: {
    disableSignalHandlers: true,
    gracePeriodMs: 15_000,
  },
  use: {
    nanocodex,
    nanocodexAuth,
  },
});
