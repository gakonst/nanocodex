import { createRequire } from 'node:module';
import { createXai } from '../runtime/xai.mjs';
import { createNodeHost } from './host.mjs';
import { createBrowserHost } from '../browser/host.mjs';

function createCodexHost(options) {
  // Function-backed Web API transports retain their own callbacks and evaluator.
  if (options.hostAuth || options.createResponse || options.createWebSocket || options.WebSocketImpl) {
    if (options.toolMode === 'code' && typeof globalThis.Worker !== 'function' && options.codeEvaluator === undefined) throw new TypeError('host-managed Codex Code Mode requires an explicit codeEvaluator');
    return createBrowserHost(options);
  }
  return createNodeHost(options);
}

/** Explicit Xai Responses runtime; no authentication or tools are inferred. */
export function create(options) {
  return createXai(options, async (module) => {
    if (module === undefined) return createRequire(import.meta.url)('../pkg-node/nanocodex.js').Nanoxai;
    const wasm = await import('../pkg-web/nanocodex.js');
    const { initializeBrowserEngine } = await import('../browser/engine.mjs');
    await initializeBrowserEngine({ module });
    return wasm.Nanoxai;
  }, 'node', { createCodexHost });
}
