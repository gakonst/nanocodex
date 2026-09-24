import { DurableObject } from 'cloudflare:workers';
export class TinyProbe extends DurableObject {
  async activationProbe() {
    const enteredAt = Date.now();
    await this.ctx.storage.deleteAll();
    return enteredAt;
  }
}
export default {
  async fetch(request, env) {
    if (!env.BENCH_TOKEN || request.method !== 'POST' || new URL(request.url).pathname !== '/activation'
      || request.headers.get('authorization') !== `Bearer ${env.BENCH_TOKEN}`) {
      return new Response('Not found', { status: 404 });
    }
    const id = env.TINY.newUniqueId();
    const startedAt = Date.now();
    const started = performance.now();
    try {
      const enteredAt = await env.TINY.get(id).activationProbe();
      return Response.json({dispatch_ms: Math.round(performance.now()-started), before_constructor_ms: enteredAt-startedAt},
        {headers:{'cache-control':'no-store'}});
    } catch {
      return Response.json({error:'probe_failed'}, {status:503});
    }
  }
};
