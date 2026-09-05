import module from "../pkg-web/nanocodex_bg.wasm";
import { bindAgent } from "./Agent.mjs";

/** Cloudflare Durable Object Agent backed by the package's compiled WASM. */
export const Agent = bindAgent(module);
export { cloudflareEgress } from "./egress.mjs";
