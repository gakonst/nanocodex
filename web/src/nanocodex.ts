import { createConfig } from "nanocodex-react";
import type { TuiCommand, TuiMessage } from "nanocodex-tui";

/** Website-owned wiring for the publishable React package. */
export const nanocodexConfig = createConfig<TuiCommand, TuiMessage>({
  worker: () => new Worker(new URL("./agent.worker.ts", import.meta.url), { type: "module" }),
});
