import assert from "node:assert/strict";
import test from "node:test";

import { ChatGptSubscription } from "../worker/index.mjs";

test("Worker subscription initialization requires the caller's compiled WASM module", async () => {
  await assert.rejects(
    ChatGptSubscription.open({
      id: "worker-subscription",
      store: {
        load: async () => ({ revision: "0" }),
        compareAndSwap: async () => ({ status: "committed", revision: "1" }),
      },
    }),
    /requires a precompiled WASM module/,
  );
});
