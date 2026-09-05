import init, { ChatGptSubscription as WasmChatGptSubscription } from "../pkg-web/nanocodex.js";

import { installHostBridge } from "../internal.mjs";
import { openSubscription } from "../runtime/chatgpt-subscription.mjs";

let initialization;

/** Opens the Rust-owned ChatGPT lifecycle in a module Worker. */
export async function open(options) {
  installHostBridge();
  if (options?.module === undefined) {
    throw new TypeError("Worker ChatGptSubscription requires a precompiled WASM module");
  }
  await initialize(options.module);
  return openSubscription(
    options,
    (config) => WasmChatGptSubscription.open(config),
    // A Cloudflare isolate can outlive an evicted Durable Object instance.
    // Rebind its stable subscription ID to the reconstructed instance.
    { replaceHost: true },
  );
}

function initialize(module) {
  return initialization ??= init({ module_or_path: module }).catch((error) => {
    initialization = undefined;
    throw error;
  });
}
