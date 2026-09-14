import { QueryClient } from "@tanstack/react-query";

export const sessionQueryKey = ["session"] as const;
export const accountQueryKey = (accountId: string | undefined) => ["account", accountId ?? null] as const;

export function retryQuery(failureCount: number, error: Error): boolean {
  const status = "status" in error ? error.status : undefined;
  if (typeof status === "number" && status >= 400 && status < 500
    && ![408, 425, 429].includes(status)) return false;
  return error.name !== "AbortError" && failureCount < 2;
}

export function createAppQueryClient(): QueryClient {
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        gcTime: 10 * 60_000,
        retry: retryQuery,
        refetchOnWindowFocus: true,
        refetchOnReconnect: true,
      },
      mutations: { retry: false, gcTime: 0 },
    },
  });
  client.setQueryDefaults(["evals"], { staleTime: 0, gcTime: 30 * 60_000, refetchOnWindowFocus: false });
  return client;
}

/** Removing queries also cancels their requests, so old accounts cannot refill the cache. */
export function clearOtherAccountQueries(client: QueryClient, accountId?: string): void {
  client.removeQueries({
    predicate: ({ queryKey }) => queryKey[0] === "account"
      && (accountId === undefined || queryKey[1] !== accountId),
  });
  client.getMutationCache().clear();
}

// Route prefetches and React observers use the same browser-lifetime client.
// Route data can be prefetched before React mounts.
export const appQueryClient = createAppQueryClient();
