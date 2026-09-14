import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useAccountSession } from "./AccountSession";
import { accountResourceOptions, refreshAccountResource } from "./accountQueries";

/** Cache the wire document once; individual surfaces select their own validated view. */
export function useAccountQuery<T>(
  accountId: string | undefined,
  path: string,
  select: (value: unknown) => T,
  options: { enabled?: boolean; staleTime?: number; refetchInterval?: number | false } = {},
) {
  const client = useQueryClient();
  const refreshSession = useAccountSession().refresh;
  const query = useQuery({
    ...accountResourceOptions(accountId, path),
    ...options,
    enabled: Boolean(accountId) && options.enabled !== false,
    select,
  });
  useEffect(() => {
    if (query.error && "status" in query.error && query.error.status === 401) void refreshSession();
  }, [query.error, refreshSession]);
  const refresh = useCallback(async (options: { throwOnError?: boolean } = {}): Promise<T | undefined> => {
    if (!accountId) return;
    try {
      const value = await refreshAccountResource(client, accountId, path);
      return value === undefined ? undefined : select(value);
    } catch (error) {
      if (options.throwOnError) throw error;
      return undefined;
    }
  }, [accountId, client, path, select]);
  return { query, refresh };
}
