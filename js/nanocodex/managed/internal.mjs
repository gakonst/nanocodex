const managedAgents = new WeakMap();

/** @internal Retains the authenticated client without exposing it on the public Agent handle. */
export function registerManagedAgent(agent, client, id, options = {}) {
  managedAgents.set(agent, {
    client,
    eventAgent: agent,
    id,
    ...(options.voiceTransport === undefined ? {} : { voiceTransport: options.voiceTransport }),
  });
}

/** @internal Retains managed lifecycle ownership for a capability-bound Agent projection. */
export function registerManagedAgentAlias(agent, source, options = {}) {
  const state = managedAgent(source);
  managedAgents.set(agent, {
    ...state,
    ...(options.voiceTransport === undefined ? {} : { voiceTransport: options.voiceTransport }),
  });
}

/** @internal Resolves an optional browser media transport owned by an Agent projection. */
export function managedBrowserVoiceTransport(agent) {
  return managedAgent(agent).voiceTransport;
}

/** @internal Starts the canonical Realtime lifecycle on the selected durable Agent. */
export function startManagedRealtime(agent, voiceSessionId, operationId) {
  const { client, id } = managedAgent(agent);
  return client.json(`${agentPath(id)}/realtime/start`, {
    method: "POST",
    body: JSON.stringify({
      voice_session_id: voiceSessionId,
      operation_id: operationId,
    }),
  });
}

/** @internal Atomically routes one Rust-formatted voice delegation on the durable Agent. */
export function routeManagedRealtime(agent, voiceSessionId, operationId, input) {
  const { client, id } = managedAgent(agent);
  return client.json(`${agentPath(id)}/realtime/delegate`, {
    method: "POST",
    body: JSON.stringify({
      voice_session_id: voiceSessionId,
      operation_id: operationId,
      input,
    }),
  });
}

/** @internal Ends the canonical Realtime lifecycle on the selected durable Agent. */
export function stopManagedRealtime(agent, voiceSessionId, operationId) {
  const { client, id } = managedAgent(agent);
  return client.json(`${agentPath(id)}/realtime/stop`, {
    method: "POST",
    body: JSON.stringify({
      voice_session_id: voiceSessionId,
      operation_id: operationId,
    }),
  });
}

/** @internal Cancels only a managed turn that this voice session started. */
export function cancelManagedTurn(agent, turnId) {
  const { client, id } = managedAgent(agent);
  return client.json(`${agentPath(id)}/turns/${encodeURIComponent(turnId)}/cancel`, {
    method: "POST",
  });
}

/** @internal Streams only canonical typed Agent events from the durable envelope log. */
export function observeManagedAgentEvents(agent, listener) {
  if (typeof listener !== "function") throw new TypeError("managed voice event listener must be a function");
  const controller = new AbortController();
  void (async () => {
    try {
      const { eventAgent } = managedAgent(agent);
      for await (const envelope of eventAgent.events.watch({
        cursor: "latest",
        signal: controller.signal,
      })) {
        if (controller.signal.aborted) return;
        if (envelope.data.type === "event") {
          listener(Object.freeze({ event: envelope.data.event, turnId: envelope.turnId }));
        }
      }
    } catch (error) {
      if (!controller.signal.aborted) reportError(error);
    }
  })();
  return () => controller.abort();
}

function managedAgent(agent) {
  const state = managedAgents.get(agent);
  if (!state) throw new TypeError("voice requires a managed Nanocodex Agent");
  return state;
}

function agentPath(id) {
  return `/v1/agents/${encodeURIComponent(id)}`;
}

function reportError(error) {
  if (typeof globalThis.reportError === "function") globalThis.reportError(error);
  else console.error(error);
}
