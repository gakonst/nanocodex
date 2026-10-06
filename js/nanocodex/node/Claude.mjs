import { createRequire } from 'node:module';
import { createClaude } from '../runtime/claude.mjs';
import { createNodeHost } from './host.mjs';
import { createBrowserHost } from '../browser/host.mjs';

function createCodexHost(options) {
  // Function-backed Web API transports retain their own callbacks and evaluator.
  if (options.hostAuth || options.createResponse || options.createWebSocket || options.WebSocketImpl) {
    if ((options.toolMode === 'code' || options.toolMode === 'code-only') && typeof globalThis.Worker !== 'function' && options.codeEvaluator === undefined) throw new TypeError('host-managed Codex Code Mode requires an explicit codeEvaluator');
    return createBrowserHost(options);
  }
  return createNodeHost(options);
}

/** Explicit Claude Messages runtime; no authentication or tools are inferred. */
export function create(options) {
  return createClaude(options, async (module) => {
    if (module === undefined) return createRequire(import.meta.url)('../pkg-node/nanocodex.js').Nanoclaude;
    const wasm = await import('../pkg-web/nanocodex.js');
    const { initializeBrowserEngine } = await import('../browser/engine.mjs');
    await initializeBrowserEngine({ module });
    return wasm.Nanoclaude;
  }, 'node', { createCodexHost });
}
