import { DEFAULT_THEMES } from "@pierre/diffs";
import {
  WorkerPoolContextProvider,
  type WorkerInitializationRenderOptions,
  type WorkerPoolOptions,
  useWorkerPool,
} from "@pierre/diffs/react";
import DiffWorker from "@pierre/diffs/worker/worker.js?worker";
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { preloadedSyntaxLanguages } from "./syntax";

function isMobileBrowser() {
  const browserNavigator = globalThis.navigator;
  if (!browserNavigator) return false;
  return (
    browserNavigator.maxTouchPoints > 0 &&
    globalThis.matchMedia?.("(max-width: 767px), (pointer: coarse)").matches === true
  );
}

function getWorkerLimits() {
  return isMobileBrowser()
    ? { poolSize: 1, totalASTLRUCacheSize: 10 }
    : { poolSize: 3, totalASTLRUCacheSize: 100 };
}

const workerLimits = getWorkerLimits();
const hardwareConcurrency = globalThis.navigator?.hardwareConcurrency ?? 1;
const poolOptions: WorkerPoolOptions = {
  poolSize: Math.min(
    Math.max(1, hardwareConcurrency - 1),
    workerLimits.poolSize,
  ),
  totalASTLRUCacheSize: workerLimits.totalASTLRUCacheSize,
  workerFactory: () => new DiffWorker(),
};

const highlighterOptions: WorkerInitializationRenderOptions = {
  theme: DEFAULT_THEMES,
  langs: preloadedSyntaxLanguages,
  preferredHighlighter: "shiki-wasm",
};

export function PierreWorkerProvider({ children }: { children: ReactNode }) {
  return (
    <WorkerPoolContextProvider
      poolOptions={poolOptions}
      highlighterOptions={highlighterOptions}
    >
      {children}
    </WorkerPoolContextProvider>
  );
}

export function usePierreRenderer() {
  const workerPool = useWorkerPool();
  const [ready, setReady] = useState(() => workerPool?.isInitialized() ?? true);
  const readyRef = useRef(ready);

  useEffect(() => {
    return workerPool?.subscribeToStatChanges((stats) => {
      const nextReady = stats.managerState === "initialized";
      if (nextReady !== readyRef.current) {
        readyRef.current = nextReady;
        setReady(nextReady);
      }
    });
  }, [workerPool]);

  return { ready, disableWorkerPool: workerPool == null };
}
