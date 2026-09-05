import { createWorkerAgent, prepareWorkerAgent } from "./WorkerAgent.mjs";

const IDLE_SNAPSHOT = Object.freeze({
  data: undefined,
  error: undefined,
  status: "idle",
});

/** Creates the stable browser runtime consumed by framework bindings. */
export function createConfig(options = {}) {
  return createAgentConfig(options, {
    create: createWorkerAgent,
    prepare: prepareWorkerAgent,
  });
}

/** @internal Dependency-injected config constructor used by focused tests. */
export function createAgentConfig(options = {}, runtime) {
  const entries = new Map();
  const agentOptions = Object.freeze({ ...(options.agent ?? {}) });
  const defaultThreadId = nonEmptyString(agentOptions.threadId)
    ?? nonEmptyString(agentOptions.sessionId)
    ?? randomId();
  const retry = nonNegativeInteger(options.retry ?? 2, "retry");
  const retryDelay = options.retryDelay ?? ((attempt) => 400 * attempt);
  if (typeof retryDelay !== "function") throw new TypeError("retryDelay must be a function");
  const failedAgents = new WeakSet();
  const underlyingAgents = new WeakMap();
  const presentedAgents = new WeakMap();
  let destroyed = false;
  let destruction;

  function resolveThreadId(parameters = {}) {
    return nonEmptyString(parameters.threadId) ?? defaultThreadId;
  }

  function createEntry(threadId) {
    const createOptions = Object.freeze({
      ...agentOptions,
      threadId,
      ...(options.origin === undefined ? {} : { origin: options.origin }),
    });
    const entry = {
      activeSubscribers: 0,
      activeTurns: 0,
      agent: undefined,
      closing: Promise.resolve(),
      createOptions,
      generation: 0,
      key: threadId,
      refetchRequested: false,
      subscriptions: new Set(),
      operation: undefined,
      snapshot: IDLE_SNAPSHOT,
    };
    entries.set(threadId, entry);
    return entry;
  }

  function publish(entry, status, data, error) {
    const snapshot = Object.freeze({ data, error, status });
    if (
      entry.snapshot.status === snapshot.status
      && entry.snapshot.data === snapshot.data
      && entry.snapshot.error === snapshot.error
    ) return;
    entry.snapshot = snapshot;
    notify(entry.subscriptions);
  }

  function notify(subscriptions) {
    for (const subscription of [...subscriptions]) {
      if (!subscription.subscribed) continue;
      try { subscription.listener(); } catch (error) { reportError(error); }
    }
  }

  async function close(agent) {
    if (agent === undefined) return;
    const underlyingAgent = underlyingAgents.get(agent) ?? agent;
    if (failedAgents.has(underlyingAgent)) {
      disposeFailedAgent(underlyingAgent);
      return;
    }
    try {
      await underlyingAgent.session.shutdown();
    } catch (error) {
      if (!failedAgents.has(underlyingAgent)) throw error;
    }
  }

  function disposeFailedAgent(agent) {
    failedAgents.add(agent);
    try { agent.dispose(); } catch (error) { reportError(error); }
  }

  function failAgent(entry, operation, agent, error) {
    disposeFailedAgent(agent);
    const presentedAgent = presentedAgents.get(agent) ?? agent;
    if (
      destroyed
      || entry.operation !== operation
      || entry.generation !== operation.generation
      || entry.agent !== presentedAgent
    ) return;
    entry.agent = undefined;
    finishGeneration(entry, operation);
    if (entry.activeSubscribers > 0) publish(entry, "error", undefined, error);
  }

  function retireAgent(entry) {
    const agent = entry.agent;
    entry.agent = undefined;
    if (agent === undefined) return entry.closing;
    const closing = entry.closing.then(() => close(agent));
    entry.closing = closing.catch(() => {});
    return closing;
  }

  function trackDurableTurns(entry, agent) {
    if (typeof agent?.turn?.prompt !== "function") return agent;
    let presentedAgent;
    const turn = Object.freeze({
      ...agent.turn,
      prompt(options) {
        const owned = agent.turn.prompt(options);
        entry.activeTurns += 1;
        let settled = false;
        const settle = () => {
          if (settled) return;
          settled = true;
          entry.activeTurns = Math.max(0, entry.activeTurns - 1);
          if (destroyed || entry.activeTurns > 0) return;
          if (entry.refetchRequested && entry.activeSubscribers > 0) {
            start(entry, true);
          } else if (entry.activeSubscribers === 0) {
            release(entry);
          }
        };
        let completion;
        try { completion = Promise.resolve(owned.result()); }
        catch (error) { completion = Promise.reject(error); }
        void completion.then(settle, settle);
        return Object.freeze({
          ...owned,
          agent: presentedAgent,
          result() { return completion; },
        });
      },
    });
    presentedAgent = Object.freeze(
      typeof agent.extend === "function"
        ? agent.extend(() => ({ turn }))
        : { ...agent, turn },
    );
    presentedAgents.set(agent, presentedAgent);
    underlyingAgents.set(presentedAgent, agent);
    return presentedAgent;
  }

  function cancelGeneration(entry) {
    entry.generation += 1;
    const operation = entry.operation;
    entry.operation = undefined;
    operation?.controller.abort();
  }

  function isCurrent(entry, operation) {
    return !destroyed
      && entry.operation === operation
      && entry.generation === operation.generation
      && entry.activeSubscribers > 0;
  }

  function finishGeneration(entry, operation) {
    if (entry.operation !== operation) return;
    entry.operation = undefined;
    operation.controller.abort();
  }

  function start(entry, force = false) {
    if (destroyed || entry.activeSubscribers === 0) return;
    const replace = force || entry.refetchRequested;
    if (entry.activeTurns > 0) {
      // A refetch (or a new observer after a live Worker failure) requests a
      // replacement; it is not authority to abandon accepted work. Coalesce
      // the request until every eagerly observed result lease has settled.
      if (replace || entry.agent === undefined) entry.refetchRequested = true;
      return;
    }
    if (!replace && (entry.operation !== undefined || entry.agent !== undefined)) return;
    entry.refetchRequested = false;
    cancelGeneration(entry);
    const operation = {
      controller: new AbortController(),
      generation: ++entry.generation,
    };
    entry.operation = operation;
    const closing = retireAgent(entry);
    publish(entry, "pending", undefined, undefined);
    if (!isCurrent(entry, operation)) return;
    try {
      void Promise.resolve(runtime.prepare(entry.createOptions, {
        signal: operation.controller.signal,
      })).catch(() => {
        // Agent.create reports an actionable error if warmup cannot be reused.
      });
    } catch {
      // Agent.create reports an actionable error if warmup cannot be reused.
    }
    void runGeneration(entry, operation, closing);
  }

  async function runGeneration(entry, operation, closing) {
    try {
      try {
        await closing;
      } catch (error) {
        if (isCurrent(entry, operation)) publish(entry, "error", undefined, error);
        finishGeneration(entry, operation);
        return;
      }
      if (!isCurrent(entry, operation)) return;

      let candidate;
      let creationFailure;
      for (let attempt = 0; attempt <= retry; attempt += 1) {
        if (!isCurrent(entry, operation)) return;
        let attemptCandidate;
        let attemptFailure;
        try {
          attemptCandidate = await runtime.create(entry.createOptions, {
            signal: operation.controller.signal,
            onFailure(error) {
              if (attemptCandidate === undefined) {
                attemptFailure ??= error;
                return;
              }
              failAgent(entry, operation, attemptCandidate, error);
            },
          });
          candidate = attemptCandidate;
          creationFailure = attemptFailure;
          break;
        } catch (error) {
          if (!isCurrent(entry, operation)) return;
          if (attempt === retry) {
            publish(entry, "error", undefined, error);
            finishGeneration(entry, operation);
            return;
          }
          let delay;
          try {
            delay = nonNegativeNumber(retryDelay(attempt + 1, error), "retryDelay result");
          } catch (retryError) {
            if (isCurrent(entry, operation)) publish(entry, "error", undefined, retryError);
            finishGeneration(entry, operation);
            return;
          }
          if (delay > 0) {
            try {
              await wait(delay, operation.controller.signal);
            } catch {
              return;
            }
          }
        }
      }
      if (candidate === undefined) return;
      if (creationFailure !== undefined) {
        if (!isCurrent(entry, operation)) {
          disposeFailedAgent(candidate);
          return;
        }
        entry.agent = candidate;
        failAgent(entry, operation, candidate, creationFailure);
        return;
      }
      if (!isCurrent(entry, operation)) {
        await close(candidate).catch(reportError);
        return;
      }
      const ownedCandidate = trackDurableTurns(entry, candidate);
      entry.agent = ownedCandidate;
      publish(entry, "success", ownedCandidate, undefined);
    } catch (error) {
      if (isCurrent(entry, operation)) {
        publish(entry, "error", undefined, error);
        finishGeneration(entry, operation);
      }
    }
  }

  function release(entry) {
    queueMicrotask(() => {
      if (entry.activeSubscribers > 0 || destroyed) return;
      // A route subscription is only a presentation observer. Accepted turns
      // retain the Agent until their result settles; an actually idle Agent is
      // still reclaimed immediately.
      if (entry.activeTurns > 0) return;
      const closing = retireAgent(entry);
      publish(entry, "idle", undefined, undefined);
      void closing.catch(reportError).finally(() => {
        if (
          entries.get(entry.key) === entry
          && entry.activeSubscribers === 0
          && entry.subscriptions.size === 0
        ) entries.delete(entry.key);
      });
    });
  }

  const config = {
    getAgent(parameters = {}) {
      if (parameters.enabled === false || destroyed) return IDLE_SNAPSHOT;
      return entries.get(resolveThreadId(parameters))?.snapshot ?? IDLE_SNAPSHOT;
    },
    subscribeAgent(parameters = {}, listener) {
      if (typeof listener !== "function") throw new TypeError("subscribeAgent requires a listener");
      if (destroyed) return () => {};
      const threadId = resolveThreadId(parameters);
      const entry = entries.get(threadId) ?? createEntry(threadId);
      const enabled = parameters.enabled !== false;
      const subscription = { listener, subscribed: true };
      entry.subscriptions.add(subscription);
      if (enabled) {
        entry.activeSubscribers += 1;
        start(entry);
      }
      return () => {
        if (!subscription.subscribed) return;
        subscription.subscribed = false;
        if (destroyed) return;
        entry.subscriptions.delete(subscription);
        if (!enabled) {
          if (entry.subscriptions.size === 0 && entry.activeSubscribers === 0) entries.delete(entry.key);
          return;
        }
        entry.activeSubscribers -= 1;
        if (entry.activeSubscribers === 0) {
          cancelGeneration(entry);
          release(entry);
        }
      };
    },
    refetchAgent(parameters = {}) {
      if (parameters.enabled === false || destroyed) return;
      const entry = entries.get(resolveThreadId(parameters));
      if (entry !== undefined) start(entry, true);
    },
    destroy() {
      if (destruction !== undefined) return destruction;
      let rejectDestruction;
      let resolveDestruction;
      destruction = new Promise((resolve, reject) => {
        rejectDestruction = reject;
        resolveDestruction = resolve;
      });
      destroyed = true;
      const closures = [];
      const notifications = [];
      try {
        for (const entry of entries.values()) {
          cancelGeneration(entry);
          entry.snapshot = IDLE_SNAPSHOT;
          const subscriptions = [...entry.subscriptions];
          entry.subscriptions.clear();
          entry.activeSubscribers = 0;
          for (const subscription of subscriptions) {
            subscription.subscribed = false;
          }
          notifications.push(subscriptions);
          // Destruction is the explicit application boundary allowed to force
          // shutdown even when an accepted result lease does not settle.
          closures.push(retireAgent(entry));
        }
        entries.clear();
      } catch (error) {
        rejectDestruction(error);
        return destruction;
      }
      Promise.all(closures).then(resolveDestruction, rejectDestruction);
      for (const subscriptions of notifications) notifyDestroyed(subscriptions);
      return destruction;
    },
  };
  return Object.freeze(config);
}

function notifyDestroyed(subscriptions) {
  for (const subscription of subscriptions) {
    try { subscription.listener(); } catch (error) { reportError(error); }
  }
}

function reportError(error) {
  try {
    if (typeof globalThis.reportError === "function") globalThis.reportError(error);
    else globalThis.console?.error?.(error);
  } catch {}
}

function nonNegativeInteger(value, name) {
  if (!Number.isInteger(value) || value < 0) throw new TypeError(`${name} must be a non-negative integer`);
  return value;
}

function nonNegativeNumber(value, name) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new TypeError(`${name} must be a non-negative finite number`);
  }
  return value;
}

function nonEmptyString(value) {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function randomId() {
  return globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
}

function wait(duration, signal) {
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => settle(resolve), duration);
    const onAbort = () => settle(reject, signal.reason);
    signal.addEventListener("abort", onAbort, { once: true });

    function settle(complete, value) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      complete(value);
    }
  });
}
