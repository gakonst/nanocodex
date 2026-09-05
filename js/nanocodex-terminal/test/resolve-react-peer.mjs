import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const requireFromTerminal = createRequire(new URL("../package.json", import.meta.url));
const reactPeerSpecifiers = new Set([
  "react",
  "react/jsx-dev-runtime",
  "react/jsx-runtime",
]);

export async function resolve(specifier, context, nextResolve) {
  if (reactPeerSpecifiers.has(specifier)) {
    return {
      shortCircuit: true,
      url: pathToFileURL(requireFromTerminal.resolve(specifier)).href,
    };
  }
  return nextResolve(specifier, context);
}
