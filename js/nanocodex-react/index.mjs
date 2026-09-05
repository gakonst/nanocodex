"use client";

import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useInsertionEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import { Actions } from "nanocodex/browser";

export { createConfig } from "nanocodex/browser";

const NanocodexContext = createContext(null);
const IDLE_AGENT_SNAPSHOT = Object.freeze({
  data: undefined,
  error: undefined,
  status: "idle",
});
const IDLE_VOICE_SNAPSHOT = Object.freeze({
  error: undefined,
  status: "idle",
  statusText: undefined,
  transcripts: Object.freeze([]),
  voice: undefined,
});
const identity = (value) => value;

/** Supplies one caller-owned vanilla browser config to Nanocodex hooks. */
export function NanocodexProvider({ children, config }) {
  if (!config) throw new TypeError("NanocodexProvider requires a config");
  return createElement(NanocodexContext.Provider, {
    value: config,
  }, children);
}

/** Returns the Nanocodex Agent resource owned by the stable vanilla config. */
export function useNanocodex(parameters = {}) {
  const config = useConfig(parameters);
  const enabled = parameters.enabled ?? true;
  const threadId = parameters.threadId;
  const selector = parameters.selector ?? identity;
  const equalityFn = parameters.equalityFn ?? Object.is;
  if (typeof selector !== "function") throw new TypeError("useNanocodex selector must be a function");
  if (typeof equalityFn !== "function") throw new TypeError("useNanocodex equalityFn must be a function");
  const resource = useMemo(() => ({ enabled, threadId }), [enabled, threadId]);
  const subscribe = useCallback(
    (listener) => config.subscribeAgent(resource, listener),
    [config, resource],
  );
  const getSnapshot = useCallback(
    () => config.getAgent(resource),
    [config, resource],
  );
  const getServerSnapshot = useCallback(() => IDLE_AGENT_SNAPSHOT, []);
  const refetch = useCallback(() => config.refetchAgent(resource), [config, resource]);
  const selectSnapshot = useCallback(
    (snapshot) => selector(agentResource(snapshot, refetch)),
    [refetch, selector],
  );
  return useExternalStoreSelector(
    subscribe,
    getSnapshot,
    getServerSnapshot,
    selectSnapshot,
    equalityFn,
  );
}

/** Subscribes to ordered typed Agent events without retaining UI state in the SDK. */
export function useAgentEvents(agent, listener, options = {}) {
  const committed = useRef(undefined);
  const includeAllSessions = options.includeAllSessions ?? false;
  useInsertionEffect(() => {
    const descriptor = { agent, includeAllSessions, listener };
    committed.current = descriptor;
    return () => {
      if (committed.current === descriptor) committed.current = undefined;
    };
  }, [agent, includeAllSessions, listener]);
  useEffect(() => {
    if (!agent) return;
    const watcher = agent.events.watch({ includeAllSessions });
    const release = watcher.onEvent((event) => {
      const current = committed.current;
      if (
        current?.agent === agent
        && current.includeAllSessions === includeAllSessions
      ) current.listener(event);
    });
    return () => {
      release();
      watcher.off();
    };
  }, [agent, includeAllSessions]);
}

/** Adapts the Rust/WASM-owned Codex voice resource without reimplementing its lifecycle. */
export function useVoice(agent, parameters = {}) {
  const enabled = parameters.enabled ?? true;
  const resource = useMemo(
    () => agent && enabled
      ? Actions.voice.create(agent, {
          ...(parameters.voice === undefined ? {} : { voice: parameters.voice }),
          ...(parameters.callUrl === undefined ? {} : { callUrl: parameters.callUrl }),
          ...(parameters.sidebandUrl === undefined ? {} : { sidebandUrl: parameters.sidebandUrl }),
          ...(parameters.captureMicrophone === undefined
            ? {}
            : { captureMicrophone: parameters.captureMicrophone }),
          ...(parameters.beforeAgentTurn === undefined
            ? {}
            : { beforeAgentTurn: parameters.beforeAgentTurn }),
        })
      : undefined,
    [
      agent,
      enabled,
      parameters.beforeAgentTurn,
      parameters.callUrl,
      parameters.captureMicrophone,
      parameters.sidebandUrl,
      parameters.voice,
    ],
  );
  useEffect(() => () => {
    void resource?.stop();
  }, [resource]);
  const subscribe = useCallback(
    (listener) => resource?.subscribe(listener) ?? (() => {}),
    [resource],
  );
  const getSnapshot = useCallback(
    () => resource?.getSnapshot() ?? IDLE_VOICE_SNAPSHOT,
    [resource],
  );
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, () => IDLE_VOICE_SNAPSHOT);
  const unavailable = useCallback(() => Promise.reject(new Error("voice requires a ready Agent")), []);
  return useMemo(() => Object.freeze({
    ...snapshot,
    isActive: snapshot.status === "active",
    isConnecting: snapshot.status === "connecting",
    isError: snapshot.status === "error",
    isIdle: snapshot.status === "idle",
    cancel: resource?.cancel ?? (async () => false),
    start: resource?.start ?? unavailable,
    stop: resource?.stop ?? (async () => {}),
    toggle: resource?.toggle ?? unavailable,
  }), [resource, snapshot, unavailable]);
}

export function useConfig(parameters = {}) {
  const context = useContext(NanocodexContext);
  const config = parameters.config ?? context;
  if (!config) throw new Error("Nanocodex hooks must be used inside NanocodexProvider");
  return config;
}

function agentResource(snapshot, refetch) {
  return Object.freeze({
    data: snapshot.data,
    error: snapshot.error,
    status: snapshot.status,
    isError: snapshot.status === "error",
    isIdle: snapshot.status === "idle",
    isPending: snapshot.status === "pending",
    isSuccess: snapshot.status === "success",
    refetch,
  });
}

function useExternalStoreSelector(
  subscribe,
  getSnapshot,
  getServerSnapshot,
  selector,
  equalityFn,
) {
  const committed = useRef({ hasValue: false, value: undefined });
  const [getSelectedSnapshot, getSelectedServerSnapshot] = useMemo(() => {
    let hasMemo = false;
    let memoizedSnapshot;
    let memoizedSelection;

    function select(nextSnapshot) {
      if (!hasMemo) {
        hasMemo = true;
        memoizedSnapshot = nextSnapshot;
        const nextSelection = selector(nextSnapshot);
        if (
          committed.current.hasValue
          && equalityFn(committed.current.value, nextSelection)
        ) {
          memoizedSelection = committed.current.value;
          return memoizedSelection;
        }
        memoizedSelection = nextSelection;
        return memoizedSelection;
      }
      if (Object.is(memoizedSnapshot, nextSnapshot)) return memoizedSelection;
      const nextSelection = selector(nextSnapshot);
      memoizedSnapshot = nextSnapshot;
      if (equalityFn(memoizedSelection, nextSelection)) return memoizedSelection;
      memoizedSelection = nextSelection;
      return memoizedSelection;
    }

    return [
      () => select(getSnapshot()),
      () => select(getServerSnapshot()),
    ];
  }, [equalityFn, getServerSnapshot, getSnapshot, selector]);
  const selection = useSyncExternalStore(
    subscribe,
    getSelectedSnapshot,
    getSelectedServerSnapshot,
  );
  useEffect(() => {
    committed.current = { hasValue: true, value: selection };
  }, [selection]);
  return selection;
}
