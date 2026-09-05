import type { CodeViewItem } from "@pierre/diffs";
import {
  WorkerPoolContextProvider,
  useWorkerPool,
} from "@pierre/diffs/react";
import type { ReactNode } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  acquirePierreWorkerPool,
  pierreHighlighterOptions,
  pierreWorkerPoolOptions,
  preparePierreItems,
} from "./pierreWorkerResource";

export function PierreWorkerProvider({ children }: { children: ReactNode }) {
  useEffect(() => acquirePierreWorkerPool(), []);
  return (
    <WorkerPoolContextProvider
      poolOptions={pierreWorkerPoolOptions()}
      highlighterOptions={pierreHighlighterOptions}
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

  const prepareItems = useCallback(
    (items: readonly CodeViewItem<undefined>[]) => preparePierreItems(items),
    [],
  );
  return {
    disableWorkerPool: workerPool == null,
    preparationBatchSize: pierreWorkerPoolOptions().totalASTLRUCacheSize ?? 1,
    prepareItems,
    ready,
  };
}
