import type {
  ChatGptSubscriptionHandle,
  ChatGptSubscriptionOptions,
} from "../types.mjs";

/** Opens the Rust-owned ChatGPT device-login and credential lifecycle. */
export function open(
  options: Omit<ChatGptSubscriptionOptions, "module"> & { module?: never },
): Promise<ChatGptSubscriptionHandle>;
