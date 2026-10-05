import { assertRequestPolicy } from './request-policy.mjs';

// Model requests use the native accepted operation and native call index. HTTP
// attempts never allocate identities. Event forwarding can race HTTP opening,
// so rendezvous with the exact session rather than guessing a latest turn.
export function createRequestPolicyHost(rootPolicy) {
  if (rootPolicy !== undefined) assertRequestPolicy(rootPolicy);
  const policies = new Map();
  const turns = new Map();
  const boundaries = new Map();
  const waiters = new Set();
  function notify() { for (const waiter of [...waiters]) waiter.check(); }
  function boundary(sessionId) {
    const value = boundaries.get(sessionId);
    if (!value) throw new Error('native model request boundary unavailable');
    return value;
  }
  return {
    bind(sessionId) { if (rootPolicy && !policies.has(sessionId)) policies.set(sessionId, rootPolicy); },
    policy(sessionId) { return policies.get(sessionId); },
    observe(encoded) {
      if (!rootPolicy) return;
      const event = typeof encoded === 'string' ? JSON.parse(encoded) : encoded;
      const payload = event?.payload;
      if (event.type === 'input.accepted' && payload?.kind === 'prompt') {
        const { session_id: sessionId, turn_id: turnId, request_id: operationId } = payload;
        if (typeof sessionId !== 'string' || typeof turnId !== 'string') return;
        const id = typeof operationId === 'string' && operationId ? operationId
          : operationId === null && payload.item_id === turnId + ':prompt' ? 'non-durable:' + payload.item_id : undefined;
        if (!id) return;
        // Native sessions serialize turns. Keep only active operation metadata.
        for (const [key, turn] of turns) if (turn.sessionId === sessionId) turns.delete(key);
        turns.set(turnId, { sessionId, operationId: id });
        boundaries.delete(sessionId);
      } else if (event.type === 'model.call.started') {
        const turn = turns.get(payload?.turn_id);
        if (!turn || !Number.isSafeInteger(payload.call_index) || payload.call_index < 1) return;
        boundaries.set(turn.sessionId, { requestId: `${turn.operationId}/model-${payload.call_index}`,
          ...(payload.call_index > 1 ? { continuationOf: `${turn.operationId}/model-${payload.call_index - 1}` } : {}) });
      }
      notify();
    },
    async resolve(sessionId, signal) {
      signal?.throwIfAborted();
      if (boundaries.has(sessionId)) return boundary(sessionId);
      if (waiters.size >= 128) throw new Error('native model request boundary wait limit exceeded');
      return new Promise((resolve, reject) => {
        let timer;
        const waiter = { sessionId, check() { if (boundaries.has(sessionId)) settle(null, boundary(sessionId)); },
          cancel() { settle(new Error('native model request boundary interrupted')); } };
        function settle(error, value) {
          if (!waiters.delete(waiter)) return;
          clearTimeout(timer); signal?.removeEventListener('abort', abort);
          if (error) reject(error); else resolve(value);
        }
        const abort = () => settle(signal.reason ?? new Error('native model request boundary cancelled'));
        waiters.add(waiter);
        signal?.addEventListener('abort', abort, { once: true });
        timer = setTimeout(() => settle(new Error('native model request boundary unavailable; update the native runtime')), 1000);
        if (signal?.aborted) abort(); else waiter.check();
      });
    },
    async fetch(sessionId, send, family, input, init) {
      const policy = policies.get(sessionId);
      if (!rootPolicy) return send(input, init);
      if (!policy) throw new Error('branch request policy unavailable');
      const context = await this.resolve(sessionId, init?.signal ?? input?.signal);
      return policy.fetch(send, family, () => context)(input, init);
    },
    async fork(sourceId, sessionId, at) {
      if (!rootPolicy) return;
      if (at !== undefined) throw new Error('request policy historical forks require an explicit policy history boundary');
      const source = policies.get(sourceId);
      if (!source) throw new Error('source branch request policy unavailable');
      policies.set(sessionId, await source.forkSession(sessionId));
    },
    release(sessionId) {
      boundaries.delete(sessionId); policies.delete(sessionId);
      for (const [key, turn] of turns) if (turn.sessionId === sessionId) turns.delete(key);
      for (const waiter of [...waiters]) if (waiter.sessionId === sessionId) waiter.cancel();
    },
  };
}
