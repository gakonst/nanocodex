import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { responseFailure } from "./accountSessionRequest.ts";
import { accountQueryKey } from "./queryClient.ts";

export function accountResourceKey(accountId: string | undefined, path: string) {
  return [...accountQueryKey(accountId), path] as const;
}

export function accountResourceOptions(accountId: string | undefined, path: string) {
  return queryOptions({
    queryKey: accountResourceKey(accountId, path),
    enabled: Boolean(accountId),
    queryFn: async ({ signal }): Promise<unknown> => {
      if (!accountId) throw new Error("Account session unavailable.");
      const response = await fetch(path, {
        signal,
        cache: "no-store",
        credentials: "same-origin",
        headers: { accept: "application/json" },
      });
      if (!response.ok) throw await responseFailure(response, "Account service unavailable.");
      return response.json();
    },
  });
}

export async function refreshAccountResource(client: QueryClient, accountId: string, path: string): Promise<unknown> {
  const queryKey = accountResourceKey(accountId, path);
  // An old mutation must not recreate a removed account's cache.
  if (!client.getQueryState(queryKey)) return undefined;
  await client.cancelQueries({ queryKey, exact: true });
  await client.invalidateQueries({ queryKey, exact: true, refetchType: "none" });
  if (!client.getQueryState(queryKey)) return undefined;
  return client.fetchQuery(accountResourceOptions(accountId, path));
}
