import { useQuery, useQueryClient } from "@tanstack/react-query";
import { clearOtherAccountQueries, sessionQueryKey } from "./queryClient";
import { sessionQueryOptions, type BrowserSession } from "./sessionQueries";
import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import type { AccountSelection } from "nanocodex-connect-ui/AccountChooser";
import {
  getCurrentUser,
  type AuthenticatedAccount,
} from "./accountSessionRequest";
import { logoutBrowserAccountSession } from "nanocodex-connect-ui/browserAccountSession";
import { clientFailureMessage } from "./clientFailure";

export { isRecord, responseFailure } from "./accountSessionRequest";
export type { AuthenticatedAccount } from "./accountSessionRequest";

type SessionStatus = "checking" | "ready" | "error";
type AccountOperation = "sign-in" | "sign-out";

type AccountSession = Readonly<{
  status: SessionStatus;
  account: AuthenticatedAccount | null;
  error: string | null;
  operation: AccountOperation | null;
  chooseAccount: (selection: AccountSelection) => Promise<void>;
  refresh: () => Promise<void>;
  signOut: () => Promise<void>;
  reauthenticationRequired: boolean;
}>;

const AccountSessionContext = createContext<AccountSession | null>(null);

export function AccountSessionProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const query = useQuery(sessionQueryOptions(queryClient));
  const user = query.data?.account ?? null;
  const status: SessionStatus = query.isPending ? "checking" : query.isError ? "error" : "ready";
  const [operationError, setError] = useState<string | null>(null);
  const error = operationError ?? (query.error ? accountFailure(query.error, "Couldn’t check your account session.") : null);
  const [operation, setOperation] = useState<AccountOperation | null>(null);
  const reauthenticationRequired = query.data?.reauthenticationRequired ?? false;
  const refresh = useCallback(async () => {
    setError(null);
    await queryClient.invalidateQueries({ queryKey: sessionQueryKey }, { cancelRefetch: false });
  }, [queryClient]);

  const acceptSession = useCallback(async (account: AuthenticatedAccount | null) => {
    await queryClient.cancelQueries({ queryKey: sessionQueryKey });
    clearOtherAccountQueries(queryClient, account?.id);
    queryClient.setQueryData<BrowserSession>(sessionQueryKey, { account, reauthenticationRequired: false });
  }, [queryClient]);

  const chooseAccount = useCallback(async (selection: AccountSelection) => {
    setOperation("sign-in");
    setError(null);
    try {
      if (selection.authentication !== "sms_otp") throw new Error("SMS verification is required.");
      const nextUser = await getCurrentUser();
      if (!nextUser?.persistent) throw new Error("The SMS account session was not created.");
      await acceptSession(nextUser);
    } catch (cause) {
      setError(accountFailure(cause, "Couldn’t sign in by SMS. Try again."));
    } finally {
      setOperation(null);
    }
  }, [acceptSession]);
  const signOut = useCallback(async () => {
    setOperation("sign-out");
    setError(null);
    try {
      await queryClient.cancelQueries({ queryKey: sessionQueryKey });
      await logoutBrowserAccountSession();
      await acceptSession(null);
      await refresh();
    } catch (cause) {
      setError(accountFailure(cause, "Couldn’t sign out. Try again."));
    } finally {
      setOperation(null);
    }
  }, [acceptSession, queryClient, refresh]);

  const value = useMemo<AccountSession>(() => ({
    account: user,
    status,
    error,
    operation,
    chooseAccount,
    refresh,
    signOut,
    reauthenticationRequired,
  }), [chooseAccount, error, operation, reauthenticationRequired, refresh, signOut, status, user]);

  return (
    <AccountSessionContext.Provider value={value}>
      {children}
    </AccountSessionContext.Provider>
  );
}

export function useAccountSession(): AccountSession {
  const session = useContext(AccountSessionContext);
  if (!session) throw new Error("useAccountSession must be used within AccountSessionProvider");
  return session;
}

function accountFailure(cause: unknown, fallback: string): string {
  return clientFailureMessage(cause, fallback);
}
