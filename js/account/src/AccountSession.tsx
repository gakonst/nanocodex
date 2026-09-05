import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { AccountSelection } from "nanocodex-connect-ui/AccountChooser";
import {
  getCurrentUser,
  isRecord,
  ReauthenticationRequiredError,
  responseFailure,
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
  const [status, setStatus] = useState<SessionStatus>("checking");
  const [user, setUser] = useState<AuthenticatedAccount | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [operation, setOperation] = useState<AccountOperation | null>(null);
  const [reauthenticationRequired, setReauthenticationRequired] = useState(false);
  const requestId = useRef(0);
  const refreshRequest = useRef<Promise<void> | undefined>(undefined);
  const refresh = useCallback((): Promise<void> => {
    if (refreshRequest.current) return refreshRequest.current;
    const currentRequest = ++requestId.current;
    let current!: Promise<void>;
    current = getCurrentUser().then(
      (nextUser) => {
        if (requestId.current !== currentRequest) return;
        setUser(nextUser);
        setStatus("ready");
        setError(null);
        setReauthenticationRequired(false);
      },
      (cause: unknown) => {
        if (requestId.current !== currentRequest) return;
        if (cause instanceof ReauthenticationRequiredError) {
          setUser(null);
          setStatus("ready");
          setError(null);
          setReauthenticationRequired(true);
          return;
        }
        setStatus("error");
        setError(accountFailure(cause, "Couldn’t check your account session."));
      },
    ).finally(() => {
      if (refreshRequest.current === current) refreshRequest.current = undefined;
    });
    refreshRequest.current = current;
    return current;
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const chooseAccount = useCallback(async (selection: AccountSelection) => {
    setOperation("sign-in");
    setError(null);
    try {
      if (selection.authentication !== "sms_otp") throw new Error("SMS verification is required.");
      const nextUser = await getCurrentUser();
      if (!nextUser?.persistent) throw new Error("The SMS account session was not created.");
      requestId.current++;
      setUser(nextUser);
      setStatus("ready");
      setReauthenticationRequired(false);
    } catch (cause) {
      setError(accountFailure(cause, "Couldn’t sign in by SMS. Try again."));
    } finally {
      setOperation(null);
    }
  }, []);
  const signOut = useCallback(async () => {
    setOperation("sign-out");
    setError(null);
    try {
      await logoutBrowserAccountSession();
      const nextUser = await getCurrentUser();
      requestId.current++;
      setUser(nextUser);
      setStatus("ready");
      setReauthenticationRequired(false);
    } catch (cause) {
      setError(accountFailure(cause, "Couldn’t sign out. Try again."));
    } finally {
      setOperation(null);
    }
  }, []);

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
