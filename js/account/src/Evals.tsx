import {
  QueryClient,
  QueryClientProvider,
  QueryErrorResetBoundary,
  queryOptions,
  useQueryClient,
  useSuspenseQueries,
  useSuspenseQuery,
} from "@tanstack/react-query";
import { Component, useDeferredValue, type ErrorInfo, type ReactNode } from "react";
import { Link, useLocation } from "react-router";
import { evalRouteFromPath, type EvalRoute } from "./evalRoute";
import {
  evalApi,
  type EvalSummary,
  type EvalWorksetDetail,
} from "./evalApi";
import { createEvalQueryClient } from "./evalQueryClient";
import {
  LiveEvals,
  type EvalSurfaceStatus,
} from "./LiveEvals";

const activeOverviewPollMs = 2_000;
const quietOverviewPollMs = 30_000;
const detailPollMs = 15_000;
const resultStaleMs = 30_000;
const resultCacheMs = 30 * 60_000;
const hoverFreshMs = 2_000;

const queryClient = createEvalQueryClient();

export async function preloadEvalOverview(): Promise<void> {
  const [overview, cluster] = overviewQueryOptions();
  await Promise.all([
    queryClient.prefetchQuery(overview),
    queryClient.prefetchQuery(cluster),
  ]);
}

type EvalRouteErrorBoundaryProps = {
  children: ReactNode;
  onReset: () => void;
};

type EvalRouteErrorBoundaryState = {
  error: Error | null;
};

class EvalRouteErrorBoundary extends Component<
  EvalRouteErrorBoundaryProps,
  EvalRouteErrorBoundaryState
> {
  state: EvalRouteErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): EvalRouteErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Evaluation surface failed", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="live-evals-boot page-grid" role="alert">
        <p className="eyebrow">Nanocodex · durable evaluations</p>
        <h1>Evals unavailable</h1>
        <p>{this.state.error.message}</p>
        <div className="eval-error-actions">
          <button
            type="button"
            onClick={() => {
              this.props.onReset();
              this.setState({ error: null });
            }}
          >
            Retry
          </button>
          <Link to="/evals">All evals</Link>
        </div>
      </div>
    );
  }
}

function summaryComplete(summary: EvalSummary) {
  return summary.running === 0 && summary.unclaimed === 0;
}

function overviewQueryOptions() {
  return [
    queryOptions({
      queryKey: ["evals", "overview"],
      queryFn: ({ signal }) => evalApi.overview(signal),
      refetchInterval: (query) => {
        const data = query.state.data;
        return data && summaryComplete(data.summary)
          ? quietOverviewPollMs
          : activeOverviewPollMs;
      },
      refetchIntervalInBackground: false,
      refetchOnMount: true,
      refetchOnWindowFocus: "always" as const,
      refetchOnReconnect: "always" as const,
      staleTime: hoverFreshMs,
    }),
    queryOptions({
      queryKey: ["evals", "cluster"],
      queryFn: ({ signal }) => evalApi.cluster(signal),
      refetchInterval: 10_000,
      refetchIntervalInBackground: false,
      refetchOnWindowFocus: "always" as const,
      refetchOnReconnect: "always" as const,
      staleTime: 5_000,
    }),
  ] as const;
}

function cachedWorksetComplete(worksetId: string, queryClient: QueryClient) {
  const detail = queryClient.getQueryData<EvalWorksetDetail>([
    "evals",
    "workset",
    worksetId,
  ]);
  return detail ? summaryComplete(detail.workset.summary) : false;
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "Evaluation refresh failed.";
}

function surfaceStatus(
  observedAtMs: number,
  queries: Array<{
    error: unknown;
    isRefetchError: boolean;
    refetch: () => Promise<unknown>;
  }>,
): EvalSurfaceStatus {
  const failed = queries.find((query) => query.isRefetchError);
  return {
    observedAtMs,
    error: failed ? errorMessage(failed.error) : null,
    retry() {
      void Promise.all(queries.map((query) => query.refetch()));
    },
  };
}

function OverviewRoute() {
  const [overviewQuery, clusterQuery] = useSuspenseQueries({
    queries: overviewQueryOptions(),
  });
  return (
    <LiveEvals
      data={{
        kind: "overview",
        overview: overviewQuery.data,
        cluster: clusterQuery.data,
      }}
      status={surfaceStatus(
        Math.min(overviewQuery.data.observedAtMs, clusterQuery.data.observedAtMs),
        [overviewQuery, clusterQuery],
      )}
    />
  );
}

function WorksetRoute({ route }: { route: Extract<EvalRoute, { kind: "workset" }> }) {
  const queryClient = useQueryClient();
  const [worksetQuery, analyticsQuery] = useSuspenseQueries({
    queries: [
      queryOptions({
        queryKey: ["evals", "workset", route.worksetId],
        queryFn: ({ signal }) => evalApi.workset(route.worksetId, signal),
        refetchInterval: (query) => {
          const data = query.state.data;
          return data && summaryComplete(data.workset.summary) ? false : 10_000;
        },
        refetchIntervalInBackground: false,
        refetchOnWindowFocus: "always" as const,
        refetchOnReconnect: "always" as const,
      }),
      queryOptions({
        queryKey: ["evals", "analytics", route.worksetId],
        queryFn: ({ signal }) => evalApi.worksetAnalytics(route.worksetId, signal),
        refetchInterval: () => cachedWorksetComplete(route.worksetId, queryClient)
          ? false
          : detailPollMs,
        staleTime: resultStaleMs,
        gcTime: resultCacheMs,
        refetchOnWindowFocus: "always" as const,
        refetchIntervalInBackground: false,
        refetchOnReconnect: "always" as const,
      }),
    ],
  });
  return (
    <LiveEvals
      data={{
        kind: "workset",
        detail: worksetQuery.data,
        analytics: analyticsQuery.data,
      }}
      status={surfaceStatus(
        Math.min(worksetQuery.data.observedAtMs, analyticsQuery.data.observedAtMs),
        [worksetQuery, analyticsQuery],
      )}
    />
  );
}

function TaskRoute({ route }: { route: Extract<EvalRoute, { kind: "task" }> }) {
  const taskQuery = useSuspenseQuery({
    queryKey: ["evals", "task", route.worksetId, route.taskId],
    queryFn: ({ signal }) => evalApi.task(route.worksetId, route.taskId, signal),
    refetchInterval: (query) => {
      const data = query.state.data;
      return data && summaryComplete(data.workset.summary) ? false : detailPollMs;
    },
    refetchIntervalInBackground: false,
    staleTime: resultStaleMs,
    gcTime: resultCacheMs,
    refetchOnWindowFocus: "always",
    refetchOnReconnect: "always",
  });
  return (
    <LiveEvals
      data={{
        kind: "task",
        snapshot: taskQuery.data,
      }}
      status={surfaceStatus(taskQuery.data.observedAtMs, [taskQuery])}
    />
  );
}

function UnknownRoute() {
  return (
    <div className="live-evals-boot page-grid" role="alert">
      <p className="eyebrow">Nanocodex · durable evaluations</p>
      <h1>Eval view not found</h1>
      <p>Return to Evals and choose a retained workset.</p>
      <div className="eval-error-actions"><Link to="/evals">All evals</Link></div>
    </div>
  );
}

function EvalsContent({ route }: { route: EvalRoute }) {
  if (route.kind === "overview") return <OverviewRoute />;
  if (route.kind === "workset") {
    return <WorksetRoute route={route} />;
  }
  if (route.kind === "task") {
    return <TaskRoute route={route} />;
  }
  return <UnknownRoute />;
}

export function Evals() {
  const location = useLocation();
  const pathname = useDeferredValue(location.pathname);
  return (
    <QueryClientProvider client={queryClient}>
      <QueryErrorResetBoundary>
        {({ reset }) => (
          <EvalRouteErrorBoundary key={pathname} onReset={reset}>
            <EvalsContent route={evalRouteFromPath(pathname)} />
          </EvalRouteErrorBoundary>
        )}
      </QueryErrorResetBoundary>
    </QueryClientProvider>
  );
}
