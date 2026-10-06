// Join trusted accepted/run events to the exact tool ABI identity. Event turn
// UUIDs and ABI turn IDs differ; neither is guessed from an ordinal or call ID.
export function createTurnLifecycle(endTurn) {
  const accepted = new Map();
  const eventCalls = new Map();
  const invocations = new Map();
  const turns = new Map();
  const key = (session, call) => JSON.stringify([session, call]);
  function join(callKey) {
    const projected = eventCalls.get(callKey);
    const invocation = invocations.get(callKey);
    if (!projected || !invocation) return;
    let owners = turns.get(projected);
    if (!owners) { owners = new Map(); turns.set(projected, owners); }
    owners.set(key(invocation.sessionId, invocation.turnId), invocation);
    eventCalls.delete(callKey);
    invocations.delete(callKey);
  }
  return {
    call(sessionId, callId, turnId, child) {
      if (typeof turnId !== 'string' || !turnId) return;
      // A call without a trusted accepted session cannot own terminal cleanup.
      if (![...accepted.values()].includes(sessionId)) return;
      const callKey = key(sessionId, callId);
      invocations.set(callKey, { sessionId, turnId, child });
      join(callKey);
    },
    observe(encoded) {
      const event = typeof encoded === 'string' ? JSON.parse(encoded) : encoded;
      const payload = event?.payload;
      const projected = payload?.turn_id;
      if (typeof projected !== 'string') return;
      if (event.type === 'input.accepted' && payload.kind === 'prompt' && typeof payload.session_id === 'string') {
        accepted.set(projected, payload.session_id);
      } else if (event.type === 'tool.call') {
        const sessionId = accepted.get(projected);
        if (!sessionId || typeof payload.call_id !== 'string') return;
        if (payload.session_id !== undefined && payload.session_id !== sessionId) return;
        const callKey = key(sessionId, payload.call_id);
        eventCalls.set(callKey, projected);
        join(callKey);
      } else if (event.type === 'run.completed' || event.type === 'run.failed') {
        const owners = turns.get(projected);
        turns.delete(projected);
        accepted.delete(projected);
        for (const [callKey, turn] of eventCalls) if (turn === projected) eventCalls.delete(callKey);
        // Ownership is removed before dispatch: uncertain cleanup is never retried.
        for (const owner of owners?.values() ?? []) {
          const hook = event.type === 'run.completed' && payload.status === 'completed'
            ? owner.child ? 'SubagentStop' : 'Stop' : 'Interrupt';
          void Promise.resolve(endTurn(owner.sessionId, owner.turnId, hook)).catch(error => {
            console.warn('Tool turn cleanup failed; not retried', error instanceof Error ? error.message : String(error));
          });
        }

      }
    },
    release(sessionId) {
      for (const [id, session] of accepted) if (session === sessionId) {
        accepted.delete(id); turns.delete(id);
        for (const [callKey, projected] of eventCalls) if (projected === id) eventCalls.delete(callKey);
      }
      for (const [callKey, invocation] of invocations) if (invocation.sessionId === sessionId) invocations.delete(callKey);
    },
    reset() { accepted.clear(); eventCalls.clear(); invocations.clear(); turns.clear(); },
  };
}
