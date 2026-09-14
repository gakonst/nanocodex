import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { getCurrentUser, ReauthenticationRequiredError, type AuthenticatedAccount } from "./accountSessionRequest.ts";
import { clearOtherAccountQueries, sessionQueryKey } from "./queryClient.ts";

export type BrowserSession = { account: AuthenticatedAccount | null; reauthenticationRequired: boolean };

export function sessionQueryOptions(client: QueryClient) {
  return queryOptions({
    queryKey: sessionQueryKey,
    staleTime: 60_000,
    queryFn: async ({ signal }): Promise<BrowserSession> => {
      let session: BrowserSession;
      try {
        session = { account: await getCurrentUser(fetch, signal), reauthenticationRequired: false };
      } catch (error) {
        if (!(error instanceof ReauthenticationRequiredError)) throw error;
        session = { account: null, reauthenticationRequired: true };
      }
      signal.throwIfAborted();
      if (client.getQueryData<BrowserSession>(sessionQueryKey)?.account?.id !== session.account?.id
        || !session.account) clearOtherAccountQueries(client, session.account?.id);
      return session;
    },
  });
}
