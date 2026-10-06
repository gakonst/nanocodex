import { applyBrowserPatch } from "../../nanocodex/pkg-web/nanocodex.js";
import { bindHostSession, releaseHostSession } from "../../nanocodex/internal.mjs";

// A private invocation route reuses the shipped Rust parser without replacing
// any root/child agent host. The caller supplies authority-checked durable IO.
export async function executeNativePatch(patch, host) {
  const route = `managed-patch:${crypto.randomUUID()}`;
  try {
    bindHostSession(host, route);
    return await applyBrowserPatch(patch, route);
  } finally { releaseHostSession(host, route); }
}
