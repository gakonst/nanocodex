import type {
  ChatGptSubscriptionHandle,
  ChatGptSubscriptionOptions,
} from "../types.mjs";

/** Opens the Rust-owned ChatGPT lifecycle in a module Worker. */
export function open(
  options: Omit<ChatGptSubscriptionOptions, "module"> & { module: WebAssembly.Module },
): Promise<ChatGptSubscriptionHandle>;
