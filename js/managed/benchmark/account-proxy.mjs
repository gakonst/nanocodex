import { build } from 'esbuild';
import { join } from 'node:path';

/** Actual account managed-ingress route, with fixture bootstrap falling through. */
export async function accountProxyWorker(root, { direct = false } = {}) {
  const bundle = await build({
    stdin: {
      contents: `import {routeManaged} from './worker/managedProxy.ts';
        export default {async fetch(request,env){
          return await routeManaged(request,env,new URL(request.url)) ?? env.NANOCODEX_BACKEND.fetch(request);
        }};`,
      resolveDir: join(root, 'js/account'),
    },
    bundle: true, write: false, format: 'esm', platform: 'browser', target: 'es2022',
    conditions: ['workerd'], external: ['cloudflare:*', 'node:*'], logLevel: 'silent',
  });
  return {
    name: 'account', modules: true, script: bundle.outputFiles[0].text,
    compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat', 'enable_request_signal'],
    serviceBindings: { NANOCODEX_BACKEND: 'managed' },
    ...(direct ? { durableObjects: {
      NANOCODEX_LIVE_API_KEYS: { className: 'ApiKeyRecord', scriptName: 'managed', useSQLite: true },
      NANOCODEX_LIVE_SESSIONS: { className: 'DurableAgentSession', scriptName: 'managed', useSQLite: true },
    } } : {}),
  };
}
