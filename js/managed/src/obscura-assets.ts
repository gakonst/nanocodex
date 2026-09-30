import worker from "./obscura-assets/worker.txt";
import bootstrap from "./obscura-assets/bootstrap.txt";
import quickjs from "./obscura-assets/quickjs.bin";
import dom from "./obscura-assets/dom.bin";
import type {
  ObscuraBrowserBundle,
  ObscuraBrowserOptions,
} from "./obscura-browser";

/** Static build-owned assets; imported only when the host selects Obscura. */
export function createBundledObscuraOptions(
  network: Fetcher | undefined,
): ObscuraBrowserOptions {
  if (!network)
    throw new Error(
      "Obscura browser provider requires the OBSCURA_NETWORK binding",
    );
  const bundle: ObscuraBrowserBundle = {
    compatibilityDate: "2026-09-29",
    mainModule: "worker.js",
    modules: {
      "worker.js": { js: worker },
      "bootstrap-source": { text: bootstrap },
      "quickjs.wasm": { wasm: quickjs },
      "dom.wasm": { wasm: dom },
    },
  };
  return { bundle, network };
}
