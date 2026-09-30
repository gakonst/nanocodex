import { attachIntlBridge } from "./intl-bridge.mjs";
/** Call for each newly initialized page BEFORE its page/parser scripts execute. */
export function installCompatibility(
  page,
  { source, NativeIntl = globalThis.Intl },
) {
  const bridge = attachIntlBridge(page.vm, NativeIntl);
  try {
    page.run(source, "portable-compat.js");
  } catch (error) {
    bridge.dispose();
    throw error;
  }
  return {
    dispose() {
      bridge.dispose();
    },
  };
}
