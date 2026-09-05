import { createRequire } from "node:module";

import { installHostBridge } from "../internal.mjs";
import { openSubscription } from "../runtime/chatgpt-subscription.mjs";

const require = createRequire(import.meta.url);
let WasmChatGptSubscription;

/** Opens a Rust-managed ChatGPT subscription over caller-owned storage and fetch. */
export async function open(options) {
  if (options?.module !== undefined) {
    throw new TypeError("Node ChatGptSubscription does not accept a browser WASM module");
  }
  installHostBridge();
  WasmChatGptSubscription ||= require("../pkg-node/nanocodex.js").ChatGptSubscription;
  return openSubscription(options, (config) => WasmChatGptSubscription.open(config));
}
