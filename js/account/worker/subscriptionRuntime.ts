import {
  ChatGptSubscription,
  type ChatGptSubscriptionHandle,
  type ChatGptSubscriptionOptions,
} from "nanocodex/worker";
import wasmModule from "../../nanocodex/pkg-web/nanocodex_bg.wasm?module";

/** Cloudflare's compiled-module adapter for the host-generic subscription API. */
export function openChatGptSubscription(
  options: Omit<ChatGptSubscriptionOptions, "module">,
): Promise<ChatGptSubscriptionHandle> {
  return ChatGptSubscription.open({ ...options, module: wasmModule });
}
