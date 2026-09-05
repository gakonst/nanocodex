import { fileURLToPath } from "node:url";

const browserTools = new URL(import.meta.resolve("nanocodex/tools/browser"));
const browserSsh = fileURLToPath(
  new URL("./devTunnelsSshBrowser.mjs", browserTools),
);
const unsupportedNodeRsa = fileURLToPath(
  new URL("./unsupportedNodeRsa.mjs", browserTools),
);
const browserSprintf = fileURLToPath(
  new URL("./browserSprintf.mjs", browserTools),
);
const browserZlib = fileURLToPath(
  new URL("./browserZlib.mjs", browserTools),
);

/**
 * Keeps unreachable Node-only SSH fallbacks out of browser and Worker bundles.
 * Add this before framework plugins so nested Worker builds inherit it.
 */
export function nanocodexTools() {
  return {
    name: "nanocodex-tools",
    enforce: "pre",
    resolveId(source, importer) {
      if (source === "@microsoft/dev-tunnels-ssh") return browserSsh;
      if (source === "node-rsa") return unsupportedNodeRsa;
      if (source === "node:zlib") return browserZlib;
      // Let the compatibility module's own default import reach Vite's normal
      // CommonJS transform; every external named import resolves to this ESM
      // boundary instead of relying on consumer optimizeDeps configuration.
      if (source === "sprintf-js" && importer?.split("?", 1)[0] !== browserSprintf) {
        return browserSprintf;
      }
      return null;
    },
  };
}
