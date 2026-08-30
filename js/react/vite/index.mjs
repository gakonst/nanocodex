"use client";

import {
  automaticWebMcpConfig,
  automaticWebMcpConnection,
} from "nanocodex/vite/client";
import { useCallback, useMemo, useSyncExternalStore } from "react";
import { createConnectAgentSource } from "../cloud/connectAgentSource.mjs";

/** Uses the Agent automatically connected and attached by nanocodex(). */
export function useNanocodex(parameters = {}) {
  const subscribe = useCallback(
    (listener) => automaticWebMcpConfig.subscribeAgent(parameters, listener),
    [parameters.enabled],
  );
  const getSnapshot = useCallback(
    () => automaticWebMcpConfig.getAgent(parameters),
    [parameters.enabled],
  );
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, idleSnapshot);
  const connection = snapshot.data ? automaticWebMcpConnection() : undefined;
  const source = useMemo(
    () => snapshot.data
      ? createConnectAgentSource(snapshot.data, {
        history: connection?.grant?.visibility?.conversationHistory === true,
      })
      : undefined,
    [snapshot.data, connection],
  );
  return Object.freeze({
    data: source,
    error: snapshot.error,
    status: snapshot.status,
    isError: snapshot.status === "error",
    isIdle: snapshot.status === "idle",
    isPending: snapshot.status === "pending",
    isSuccess: snapshot.status === "success",
    refetch: () => automaticWebMcpConfig.refetchAgent(parameters),
  });
}

export { automaticWebMcpConfig as config };

const IDLE = Object.freeze({ data: undefined, error: undefined, status: "idle" });
function idleSnapshot() { return IDLE; }
