import { Check, CircleUserRound, Copy, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import {
  AccountConnectionCard,
  AccountConnectionSection,
  AccountConnectionSurface,
} from "nanocodex-connect-ui/AccountConnectionSurface";
import { isRecord, responseFailure, useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import { ConnectionLogo } from "nanocodex-connect-ui/ConnectionLogo";
import { deploymentHealth } from "./deploymentHealth";
import { localDevelopmentCredential } from "./localDevelopmentCredential";
import { ProfileConnectors } from "./ProfileConnectors";
import {
  decodeWalletBalance,
  formatDollars,
  formatWalletBalance,
  type WalletBalance,
} from "./walletFunding";
import { useWalletFunding } from "./useWalletFunding";

type ApiKeyMetadata = Readonly<{
  id: string;
  label: string;
  prefix: string;
  createdAt: number;
}>;

type NewApiKey = Readonly<{
  token: string;
  metadata: ApiKeyMetadata;
}>;

type CredentialStatus = Readonly<{
  ready: boolean;
  active: "openai" | "chatgpt" | null;
  openai: { connected: boolean };
  chatgpt: {
    connected: boolean;
    accountId?: string;
    login?: {
      verificationUrl: string;
      userCode: string;
      expiresAt: number;
      pollAfterMs: number;
    };
  };
}>;

type AccountDataRequest = Readonly<{
  accountId: string;
  promise: Promise<void>;
}>;

type WalletBalanceRequest = Readonly<{
  accountId: string;
  controller: AbortController;
  promise: Promise<boolean>;
}>;

const API_KEY_ID = /^[A-Za-z0-9_-]{12}$/;

export function AccountMenu({ inline = false }: Readonly<{ inline?: boolean }>) {
  const session = useAccountSession();
  const refreshSession = session.refresh;
  const accountId = session.account?.id;
  const accountPersistent = session.account?.persistent === true;
  const [open, setOpen] = useState(() => inline || new URL(window.location.href).searchParams.has("connector_result"));
  const walletFunding = useWalletFunding(inline || open);
  const [keys, setKeys] = useState<ApiKeyMetadata[] | null>(null);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [keyOperation, setKeyOperation] = useState<string | null>(null);
  const [newKey, setNewKey] = useState<NewApiKey | null>(null);
  const [label, setLabel] = useState("");
  const [copied, setCopied] = useState(false);
  const [walletBalance, setWalletBalance] = useState<WalletBalance | null>(null);
  const [walletBalanceError, setWalletBalanceError] = useState<string | null>(null);
  const [credentials, setCredentials] = useState<CredentialStatus | null>(null);
  const [credentialError, setCredentialError] = useState<string | null>(null);
  const [providerOperation, setProviderOperation] = useState<string | null>(null);
  const [openAiKey, setOpenAiKey] = useState("");
  const [openAiExpanded, setOpenAiExpanded] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const cachedAccountId = useRef<string | undefined>(undefined);
  const keyRequest = useRef<AccountDataRequest | undefined>(undefined);
  const credentialRequest = useRef<AccountDataRequest | undefined>(undefined);
  const walletBalanceRequest = useRef<WalletBalanceRequest | undefined>(undefined);

  const close = useCallback(() => {
    setOpen(false);
    setNewKey(null);
    setCopied(false);
  }, []);

  const loadKeys = useCallback((): Promise<void> => {
    if (!accountId) return Promise.resolve();
    if (keyRequest.current?.accountId === accountId) return keyRequest.current.promise;
    if (cachedAccountId.current === accountId) setKeyError(null);
    let current!: Promise<void>;
    current = (async () => {
      try {
        const response = await apiRequest("/v1/api-keys");
        if (response.status === 401) {
          await response.body?.cancel();
          await refreshSession();
          return;
        }
        if (!response.ok) throw await responseFailure(response, "Couldn’t load API keys.");
        const body: unknown = await response.json();
        if (!isRecord(body) || !Array.isArray(body.data)) throw new Error("Invalid API key response.");
        if (cachedAccountId.current === accountId) setKeys(body.data.map(decodeApiKey));
      } catch (cause) {
        if (cachedAccountId.current === accountId) {
          setKeyError(failureMessage(cause, "Couldn’t load API keys."));
        }
      }
    })().finally(() => {
      if (keyRequest.current?.promise === current) keyRequest.current = undefined;
    });
    keyRequest.current = { accountId, promise: current };
    return current;
  }, [accountId, refreshSession]);

  const loadCredentials = useCallback((): Promise<void> => {
    if (!accountId) return Promise.resolve();
    if (credentialRequest.current?.accountId === accountId) {
      return credentialRequest.current.promise;
    }
    if (cachedAccountId.current === accountId) setCredentialError(null);
    let current!: Promise<void>;
    current = (async () => {
      try {
        const response = await apiRequest("/v1/credentials");
        if (response.status === 401) {
          await response.body?.cancel();
          await refreshSession();
          return;
        }
        if (!response.ok) throw await responseFailure(response, "Couldn’t load model connections.");
        const nextCredentials = decodeCredentialStatus(await response.json());
        if (cachedAccountId.current === accountId) setCredentials(nextCredentials);
      } catch (cause) {
        if (cachedAccountId.current === accountId) {
          setCredentialError(failureMessage(cause, "Couldn’t load model connections."));
        }
      }
    })().finally(() => {
      if (credentialRequest.current?.promise === current) credentialRequest.current = undefined;
    });
    credentialRequest.current = { accountId, promise: current };
    return current;
  }, [accountId, refreshSession]);

  const pollChatGpt = useCallback(async () => {
    try {
      const response = await apiRequest("/v1/credentials/chatgpt/login");
      if (!response.ok) throw await responseFailure(response, "Couldn’t check ChatGPT sign-in.");
      const value: unknown = await response.json();
      if (isRecord(value) && value.state === "pending") {
        const login = decodeChatGptLogin(value);
        setCredentials((current) => current ? {
          ...current,
          chatgpt: { ...current.chatgpt, login },
        } : current);
        return;
      }
      await loadCredentials();
      notifyModelCredentialChanged();
    } catch (cause) {
      setCredentialError(failureMessage(cause, "Couldn’t check ChatGPT sign-in."));
    }
  }, [loadCredentials]);

  const loadWalletBalance = useCallback((force = false): Promise<boolean> => {
    const address = session.account?.address;
    if (!accountId || !address) return Promise.resolve(false);
    if (!force && walletBalanceRequest.current?.accountId === accountId) {
      return walletBalanceRequest.current.promise;
    }
    if (force) walletBalanceRequest.current?.controller.abort();
    const controller = new AbortController();
    setWalletBalanceError(null);
    let current!: Promise<boolean>;
    current = (async () => {
      try {
        const response = await apiRequest("/v1/wallet/balance", { signal: controller.signal });
        if (response.status === 401) {
          await response.body?.cancel();
          await refreshSession();
          return false;
        }
        if (!response.ok) throw await responseFailure(response, "Couldn’t load the Wallet balance.");
        const balance = decodeWalletBalance(await response.json(), address);
        if (cachedAccountId.current === accountId) setWalletBalance(balance);
        return true;
      } catch (cause) {
        if (!controller.signal.aborted && cachedAccountId.current === accountId) {
          setWalletBalanceError(failureMessage(cause, "Couldn’t load the Wallet balance."));
        }
        return false;
      }
    })().finally(() => {
      if (walletBalanceRequest.current?.promise === current) {
        walletBalanceRequest.current = undefined;
      }
    });
    walletBalanceRequest.current = { accountId, controller, promise: current };
    return current;
  }, [accountId, refreshSession, session.account?.address]);

  useEffect(() => {
    if (!accountId) {
      walletBalanceRequest.current?.controller.abort();
      walletBalanceRequest.current = undefined;
      cachedAccountId.current = undefined;
      setKeys(null);
      setKeyError(null);
      setNewKey(null);
      setWalletBalance(null);
      setWalletBalanceError(null);
      setCredentials(null);
      setCredentialError(null);
      return;
    }
    const accountChanged = cachedAccountId.current !== accountId;
    if (accountChanged) {
      walletBalanceRequest.current?.controller.abort();
      walletBalanceRequest.current = undefined;
      cachedAccountId.current = accountId;
      setKeys(null);
      setKeyError(null);
      setNewKey(null);
      setWalletBalance(null);
      setWalletBalanceError(null);
      setCredentials(null);
      setCredentialError(null);
    }
    if (!inline && !open) return;
    const missing: Promise<unknown>[] = [];
    if (accountChanged || keys === null) missing.push(loadKeys());
    if (accountChanged || credentials === null) missing.push(loadCredentials());
    if (accountChanged || walletBalance === null) missing.push(loadWalletBalance());
    void Promise.all(missing);
  }, [accountId, credentials, inline, keys, loadCredentials, loadKeys, loadWalletBalance, open, walletBalance]);

  useEffect(() => () => {
    walletBalanceRequest.current?.controller.abort();
  }, []);

  useEffect(() => {
    if (!accountId || !session.account?.address || (!inline && !open)) return;
    const timer = window.setInterval(() => void loadWalletBalance(), 5 * 60_000);
    return () => window.clearInterval(timer);
  }, [accountId, inline, loadWalletBalance, open, session.account?.address]);

  useEffect(() => {
    const login = credentials?.chatgpt.login;
    if ((!inline && !open) || !login) return;
    const timer = window.setTimeout(
      () => void pollChatGpt(),
      Math.max(1_000, login.pollAfterMs),
    );
    return () => window.clearTimeout(timer);
  }, [credentials?.chatgpt.login, inline, open, pollChatGpt]);

  useEffect(() => {
    if (inline || !open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [close, inline, open]);

  const createKey = async (event: FormEvent) => {
    event.preventDefault();
    if (keyOperation) return;
    setKeyOperation("create");
    setKeyError(null);
    setNewKey(null);
    try {
      const response = await apiRequest("/v1/api-keys", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ label }),
      });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, "Couldn’t create the API key.");
      const body: unknown = await response.json();
      if (!isRecord(body) || typeof body.api_key !== "string") {
        throw new Error("Invalid API key response.");
      }
      const metadata = decodeApiKey(body.key);
      setKeys((current) => [metadata, ...(current ?? []).filter((key) => key.id !== metadata.id)]);
      setNewKey({ token: body.api_key, metadata });
      setLabel("");
    } catch (cause) {
      setKeyError(failureMessage(cause, "Couldn’t create the API key."));
    } finally {
      setKeyOperation(null);
    }
  };

  const revokeKey = async (key: ApiKeyMetadata) => {
    if (keyOperation) return;
    setKeyOperation(key.id);
    setKeyError(null);
    try {
      const response = await apiRequest(`/v1/api-keys/${encodeURIComponent(key.id)}`, {
        method: "DELETE",
      });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, "Couldn’t revoke the API key.");
      await response.body?.cancel();
      setKeys((current) => current?.filter((candidate) => candidate.id !== key.id) ?? []);
      if (newKey?.metadata.id === key.id) setNewKey(null);
    } catch (cause) {
      setKeyError(failureMessage(cause, "Couldn’t revoke the API key."));
    } finally {
      setKeyOperation(null);
    }
  };

  const copyNewKey = async () => {
    if (!newKey) return;
    try {
      await navigator.clipboard.writeText(newKey.token);
      setCopied(true);
    } catch {
      setKeyError("Couldn’t copy the API key. Select and copy it manually.");
    }
  };

  const connectOpenAi = async (event: FormEvent) => {
    event.preventDefault();
    if (!openAiKey.trim() || providerOperation) return;
    setProviderOperation("openai");
    setCredentialError(null);
    try {
      const response = await apiRequest("/v1/credentials/openai", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ api_key: openAiKey.trim() }),
      });
      if (!response.ok) throw await responseFailure(response, "Couldn’t connect the OpenAI key.");
      setOpenAiKey("");
      setOpenAiExpanded(false);
      await loadCredentials();
      notifyModelCredentialChanged();
    } catch (cause) {
      setCredentialError(failureMessage(cause, "Couldn’t connect the OpenAI key."));
    } finally {
      setProviderOperation(null);
    }
  };

  const startChatGpt = async () => {
    if (providerOperation) return;
    if (localDevelopmentCredential.enabled) {
      setProviderOperation("chatgpt");
      setCredentialError(null);
      try {
        if (!accountId) throw new Error("Account session unavailable.");
        await localDevelopmentCredential.refresh(accountId);
        await loadCredentials();
        notifyModelCredentialChanged();
      } catch (cause) {
        setCredentialError(failureMessage(cause, "Couldn’t reconnect ChatGPT."));
      } finally {
        setProviderOperation(null);
      }
      return;
    }
    const popup = window.open("about:blank", "nanocodex-chatgpt-login");
    if (popup) popup.opener = null;
    setProviderOperation("chatgpt");
    setCredentialError(null);
    try {
      const response = await apiRequest("/v1/credentials/chatgpt/login", { method: "POST" });
      if (!response.ok) throw await responseFailure(response, "Couldn’t start ChatGPT sign-in.");
      const login = decodeChatGptLogin(await response.json());
      setCredentials((current) => current ? {
        ...current,
        chatgpt: { ...current.chatgpt, login },
      } : null);
      if (popup) popup.location.href = login.verificationUrl;
      else window.open(login.verificationUrl, "_blank", "noopener,noreferrer");
    } catch (cause) {
      popup?.close();
      setCredentialError(failureMessage(cause, "Couldn’t start ChatGPT sign-in."));
    } finally {
      setProviderOperation(null);
    }
  };

  const disconnectProvider = async (provider: "openai" | "chatgpt") => {
    if (providerOperation) return;
    setProviderOperation(provider);
    setCredentialError(null);
    try {
      const response = await apiRequest(`/v1/credentials/${provider}`, { method: "DELETE" });
      if (!response.ok) throw await responseFailure(response, `Couldn’t disconnect ${provider}.`);
      await response.body?.cancel();
      await loadCredentials();
      notifyModelCredentialChanged();
    } catch (cause) {
      setCredentialError(failureMessage(cause, `Couldn’t disconnect ${provider}.`));
    } finally {
      setProviderOperation(null);
    }
  };

  const accountLabel = accountPersistent && accountId ? shortIdentity(accountId) : "account";

  if (inline && session.status !== "checking" && !accountPersistent) {
    return (
      <div className="connect-onboarding terminal-sms-auth connect-route-sms-auth">
        <AccountChooser
          description={session.reauthenticationRequired
            ? "Your session expired. Enter your phone number to restore this account’s memory and connections."
            : "Verify by SMS to unlock three free Luna prompts. No ChatGPT connection is required."}
          disabled={session.operation !== null}
          failure={session.error}
          onChooseAccount={(selection) => void session.chooseAccount(selection)}
        />
      </div>
    );
  }

  if (inline && session.status !== "checking" && accountPersistent && session.account) {
    return (
      <div className="account-inline">
        <AccountConnectionSurface
          description={<>Manage the hosted connections your Nanocodex agents can use.</>}
          footer={<div className="account-wallet-session">
            <span>Account {shortIdentity(session.account.id)}</span>
            <button
              className="wizard-sign-out"
              disabled={session.operation !== null}
              onClick={() => void session.signOut()}
              type="button"
            >
              Sign out
            </button>
          </div>}
          title="Connect"
        >
          <AccountConnectionSection
            eyebrow="Service"
            meta="Available to your agents"
            title="Connections"
            titleId="connections-heading"
          >
            {session.error ? (
              <div className="account-failure" role="alert">
                <p>{session.error}</p>
                <button type="button" onClick={() => void session.refresh()}>Retry</button>
              </div>
            ) : null}
            {credentialError ? (
              <div className="account-failure" role="alert">
                <p>{credentialError}</p>
                <button type="button" onClick={() => void loadCredentials()}>Retry</button>
              </div>
            ) : null}
            <ProfileConnectors
              accountId={session.account.id}
              after={<>
                {credentials?.chatgpt.login ? (
                  <div className="new-api-key" role="status">
                    <strong>Finish ChatGPT sign-in</strong>
                    <p>Enter this code on the OpenAI page, then leave this panel open.</p>
                    <code>{credentials.chatgpt.login.userCode}</code>
                    <a href={credentials.chatgpt.login.verificationUrl} target="_blank" rel="noreferrer">Open sign-in page</a>
                  </div>
                ) : null}
                {credentials && !credentials.openai.connected && openAiExpanded ? (
                  <form className="connection-setup api-key-create" onSubmit={(event) => void connectOpenAi(event)}>
                    <label htmlFor="openai-key">OpenAI API key</label>
                    <div>
                      <input
                        autoComplete="off"
                        id="openai-key"
                        onChange={(event) => setOpenAiKey(event.target.value)}
                        placeholder="sk-…"
                        type="password"
                        value={openAiKey}
                      />
                      <button type="submit" disabled={!openAiKey.trim() || providerOperation !== null}>Host key</button>
                    </div>
                  </form>
                ) : null}
              </>}
              key={session.account.id}
              presentation="wizard"
              refreshSession={refreshSession}
            >
              <TempoWalletConnectionCard
                address={session.account.address}
                balance={walletBalanceError
                  ? walletBalance ? `${formatWalletBalance(walletBalance)} · refresh failed` : "Balance unavailable"
                  : walletBalance ? formatWalletBalance(walletBalance) : "Loading balance…"}
                fundingAmountCents={walletFunding.amountCents}
                fundingAvailable={walletFunding.available}
                fundingError={walletFunding.error}
                fundingOperation={walletFunding.operation}
                fundingSuccess={null}
                onFund={walletFunding.fund}
              />
              {credentials ? (
                <>
                  <AccountConnectionCard
                    action={credentials.chatgpt.connected ? "Disconnect" : "Connect"}
                    connected={credentials.chatgpt.connected}
                    detail={credentials.chatgpt.connected
                      ? credentials.chatgpt.accountId ?? "Connected to your ChatGPT account"
                      : "Use your ChatGPT subscription for model access"}
                    disabled={providerOperation !== null}
                    logo={<ConnectionLogo id="chatgpt" />}
                    onClick={() => void (credentials.chatgpt.connected
                      ? disconnectProvider("chatgpt")
                      : startChatGpt())}
                    title="ChatGPT"
                  />
                  <AccountConnectionCard
                    action={credentials.openai.connected ? "Disconnect" : openAiExpanded ? "Close" : "Add key"}
                    connected={credentials.openai.connected}
                    detail={credentials.openai.connected
                      ? `Hosted${credentials.active === "openai" ? " · active" : ""}`
                      : "Host a raw key for model access"}
                    disabled={providerOperation !== null}
                    logo={<ConnectionLogo id="openai" />}
                    onClick={() => void (credentials.openai.connected
                      ? disconnectProvider("openai")
                      : setOpenAiExpanded((current) => !current))}
                    title="OpenAI API key"
                  />
                </>
              ) : null}
            </ProfileConnectors>
          </AccountConnectionSection>

          <AccountConnectionSection
            eyebrow="Access"
            meta="CLI, CI, and other clients"
            title="API keys"
            titleId="api-key-heading"
          >
            {keyError ? (
              <div className="account-failure" role="alert">
                <p>{keyError}</p>
                <button type="button" onClick={() => void loadKeys()}>Retry</button>
              </div>
            ) : null}
            {keys && newKey ? (
              <div className="new-api-key" role="status">
                <strong>Copy this key now</strong>
                <p>It won’t be shown again.</p>
                <code>{newKey.token}</code>
                <div>
                  <button type="button" onClick={() => void copyNewKey()}>
                    {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                    {copied ? "Copied" : "Copy key"}
                  </button>
                  <button type="button" onClick={() => setNewKey(null)}>Dismiss</button>
                </div>
              </div>
            ) : null}
            {keys ? (
              <form className="api-key-create" onSubmit={(event) => void createKey(event)}>
                <label htmlFor="api-key-label">Nanocodex API key</label>
                <div>
                  <input
                    id="api-key-label"
                    value={label}
                    maxLength={120}
                    placeholder="CLI, CI, or laptop"
                    onChange={(event) => setLabel(event.target.value)}
                  />
                  <button type="submit" disabled={keyOperation !== null}>Create</button>
                </div>
              </form>
            ) : null}
            {keys?.length ? (
              <ul className="api-key-list">
                {keys.map((key) => (
                  <li key={key.id}>
                    <div>
                      <strong>{key.label}</strong>
                      <code>{key.prefix}…</code>
                      <time dateTime={new Date(key.createdAt).toISOString()}>
                        {new Date(key.createdAt).toLocaleDateString()}
                      </time>
                    </div>
                    <button
                      disabled={keyOperation !== null}
                      onClick={() => void revokeKey(key)}
                      type="button"
                    >
                      Revoke
                    </button>
                  </li>
                ))}
              </ul>
            ) : keys ? <p className="api-key-empty">No API keys.</p> : null}
          </AccountConnectionSection>
        </AccountConnectionSurface>
      </div>
    );
  }

  return (
    <div className={inline ? "wizard-page wizard-review-page account-inline" : "account-menu"} ref={menuRef}>
      {!inline ? (
        <button
          className="account-menu-trigger"
          type="button"
          aria-label={accountLabel}
          aria-expanded={open}
          aria-haspopup="dialog"
          onClick={() => open ? close() : setOpen(true)}
        >
          <CircleUserRound aria-hidden="true" />
          <span>{accountLabel}</span>
        </button>
      ) : null}
      {(inline || open) && session.status !== "checking" ? (
        <section className={inline ? "account-inline-panel" : "account-panel"} aria-label="Nanocodex profile">
          {inline && session.account ? (
            <header className="wizard-intro">
              <div className="wizard-app">
                <h1>Account</h1>
                <p>Signed in as {shortIdentity(session.account.id)}. Manage the same hosted connections your Nanocodex agents can use.</p>
              </div>
            </header>
          ) : null}
          {!inline ? <header className="account-panel-header">
            <div>
              <span>Profile</span>
              {session.account ? <strong>{session.account.persistent
                ? shortIdentity(session.account.id)
                : "This browser"}</strong> : null}
            </div>
            <button type="button" aria-label="Close account panel" onClick={close}>
              <X aria-hidden="true" />
            </button>
          </header> : null}

          {session.error ? (
            <div className="account-failure" role="alert">
              <p>{session.error}</p>
              <button type="button" onClick={() => void session.refresh()}>Retry</button>
            </div>
          ) : null}

          {session.account ? (
            <>
              <section className={inline ? "wizard-section account-identity" : "account-summary"}>
                {inline ? (
                  <header className="wizard-section-title">
                    <div><span>Account</span><h2>SMS identity</h2></div>
                    <small>{shortIdentity(session.account.id)}</small>
                  </header>
                ) : (
                  <>
                    <span>{session.account.persistent ? "SMS identity" : "Browser session"}</span>
                    <span>{session.account.persistent ? "Available across devices" : "Verify your phone to keep it"}</span>
                  </>
                )}
                {session.account.persistent ? (
                  <button
                    className={inline ? "wizard-sign-out" : undefined}
                    type="button"
                    disabled={session.operation !== null}
                    onClick={() => void session.signOut()}
                  >
                    Sign out
                  </button>
                ) : null}
              </section>

              {!accountPersistent ? (
                <AccountChooser
                  description="Verify your phone to keep this account and unlock connections and API keys."
                  disabled={session.operation !== null}
                  failure={session.error}
                  onChooseAccount={(selection) => void session.chooseAccount(selection)}
                />
              ) : null}

              <div className={inline ? "account-profile-content wizard-sections" : "api-key-panel account-profile-content"}>
                <section className={inline ? "wizard-section" : undefined} aria-labelledby="connections-heading">
                <div className={inline ? "wizard-section-title api-key-heading" : "api-key-heading"}>
                  <div>
                    {inline ? <span>Service</span> : null}
                    <h2 id="connections-heading">Connections</h2>
                    {!inline ? <p>{accountPersistent
                      ? "Choose a service to connect it through your private broker. Connected services can be removed from the same tile."
                      : "Verify your phone above to enable connections and API keys."}</p> : null}
                  </div>
                  {inline ? <small>Available to your agents</small> : null}
                </div>

                {credentialError ? (
                  <div className="account-failure" role="alert">
                    <p>{credentialError}</p>
                    <button type="button" onClick={() => void loadCredentials()}>Retry</button>
                  </div>
                ) : null}

                <ProfileConnectors
                  accountId={session.account.id}
                  key={session.account.id}
                  presentation={inline ? "wizard" : "profile"}
                  requiresLogin={!accountPersistent}
                  refreshSession={refreshSession}
                >
                  {credentials ? (
                    <>
                      {inline ? <AccountConnectionCard
                        action={credentials.chatgpt.connected ? "Disconnect" : "Connect"}
                        connected={credentials.chatgpt.connected}
                        detail={credentials.chatgpt.connected
                          ? credentials.chatgpt.accountId ?? "Connected to your ChatGPT account"
                          : "Use your ChatGPT subscription for model access"}
                        disabled={!accountPersistent || providerOperation !== null}
                        logo={<ConnectionLogo id="chatgpt" />}
                        onClick={() => void (credentials.chatgpt.connected
                          ? disconnectProvider("chatgpt")
                          : startChatGpt())}
                        title="ChatGPT"
                      /> : <button
                        className={`connection-card${credentials.chatgpt.connected ? " is-connected" : ""}${accountPersistent ? "" : " is-locked"}`}
                        disabled={!accountPersistent || providerOperation !== null}
                        onClick={() => void (credentials.chatgpt.connected
                          ? disconnectProvider("chatgpt")
                          : startChatGpt())}
                        type="button"
                      >
                        <ConnectionLogo id="chatgpt" />
                        <span className="connection-card-copy">
                          <strong>ChatGPT</strong>
                          <span>{credentials.chatgpt.connected
                            ? credentials.chatgpt.accountId ?? "Connected to your ChatGPT account"
                            : "Use your ChatGPT subscription for model access"}</span>
                        </span>
                        <span className="connection-card-action">
                          {credentials.chatgpt.connected ? "Disconnect" : "Connect"}
                        </span>
                      </button>}
                      {credentials.chatgpt.login ? (
                        <div className="new-api-key" role="status">
                          <strong>Finish ChatGPT sign-in</strong>
                          <p>Enter this code on the OpenAI page, then leave this panel open.</p>
                          <code>{credentials.chatgpt.login.userCode}</code>
                          <a href={credentials.chatgpt.login.verificationUrl} target="_blank" rel="noreferrer">Open sign-in page</a>
                        </div>
                      ) : null}
                      {inline ? <AccountConnectionCard
                        action={credentials.openai.connected ? "Disconnect" : openAiExpanded ? "Close" : "Add key"}
                        connected={credentials.openai.connected}
                        detail={credentials.openai.connected
                          ? `Hosted${credentials.active === "openai" ? " · active" : ""}`
                          : "Host a raw key for model access"}
                        disabled={!accountPersistent || providerOperation !== null}
                        logo={<ConnectionLogo id="openai" />}
                        onClick={() => void (credentials.openai.connected
                          ? disconnectProvider("openai")
                          : setOpenAiExpanded((current) => !current))}
                        title="OpenAI API key"
                      /> : <button
                        className={`connection-card${credentials.openai.connected ? " is-connected" : ""}${accountPersistent ? "" : " is-locked"}`}
                        disabled={!accountPersistent || providerOperation !== null}
                        onClick={() => void (credentials.openai.connected
                          ? disconnectProvider("openai")
                          : setOpenAiExpanded((current) => !current))}
                        type="button"
                      >
                        <ConnectionLogo id="openai" />
                        <span className="connection-card-copy">
                          <strong>OpenAI API key</strong>
                          <span>{credentials.openai.connected
                            ? `Hosted${credentials.active === "openai" ? " · active" : ""}`
                            : "Host a raw key for model access"}</span>
                        </span>
                        <span className="connection-card-action">
                          {credentials.openai.connected ? "Disconnect" : openAiExpanded ? "Close" : "Add key"}
                        </span>
                      </button>}
                      {!credentials.openai.connected && openAiExpanded ? (
                        <form className="connection-setup api-key-create" onSubmit={(event) => void connectOpenAi(event)}>
                          <label htmlFor="openai-key">OpenAI API key</label>
                          <div>
                            <input
                              autoComplete="off"
                              id="openai-key"
                              onChange={(event) => setOpenAiKey(event.target.value)}
                              placeholder="sk-…"
                              type="password"
                              value={openAiKey}
                            />
                            <button type="submit" disabled={!openAiKey.trim() || providerOperation !== null}>Host key</button>
                          </div>
                        </form>
                      ) : null}
                    </>
                  ) : null}
                </ProfileConnectors>
                </section>

                <section className={`${inline ? "wizard-section " : ""}account-api-keys${accountPersistent ? "" : " is-locked"}`} aria-labelledby="api-key-heading">
                  <div className={inline ? "wizard-section-title api-key-heading" : "api-key-heading"}>
                    <div>
                      {inline ? <span>Access</span> : null}
                      <h2 id="api-key-heading">API keys</h2>
                      {!inline ? <p>Create Nanocodex API keys for the CLI, CI, and other clients.</p> : null}
                    </div>
                    {inline ? <small>CLI, CI, and other clients</small> : null}
                  </div>

                  {keyError ? (
                    <div className="account-failure" role="alert">
                      <p>{keyError}</p>
                      <button type="button" onClick={() => void loadKeys()}>Retry</button>
                    </div>
                  ) : null}

                  {keys && newKey ? (
                    <div className="new-api-key" role="status">
                      <strong>Copy this key now</strong>
                      <p>It won’t be shown again.</p>
                      <code>{newKey.token}</code>
                      <div>
                        <button type="button" onClick={() => void copyNewKey()}>
                          {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                          {copied ? "Copied" : "Copy key"}
                        </button>
                        <button type="button" onClick={() => setNewKey(null)}>Dismiss</button>
                      </div>
                    </div>
                  ) : null}

                  {keys ? (
                    <form className="api-key-create" onSubmit={(event) => void createKey(event)}>
                      <label htmlFor="api-key-label">Nanocodex API key</label>
                      <div>
                        <input
                          id="api-key-label"
                          disabled={!accountPersistent}
                          value={label}
                          maxLength={120}
                          placeholder="CLI, CI, or laptop"
                          onChange={(event) => setLabel(event.target.value)}
                        />
                        <button type="submit" disabled={!accountPersistent || keyOperation !== null}>Create</button>
                      </div>
                    </form>
                  ) : null}

                  {keys?.length ? (
                    <ul className="api-key-list">
                      {keys.map((key) => (
                        <li key={key.id}>
                          <div>
                            <strong>{key.label}</strong>
                            <code>{key.prefix}…</code>
                            <time dateTime={new Date(key.createdAt).toISOString()}>
                              {new Date(key.createdAt).toLocaleDateString()}
                            </time>
                          </div>
                          <button
                            type="button"
                            disabled={!accountPersistent || keyOperation !== null}
                            onClick={() => void revokeKey(key)}
                          >
                            Revoke
                          </button>
                        </li>
                      ))}
                    </ul>
                  ) : keys ? (
                    <p className="api-key-empty">No API keys.</p>
                  ) : null}
                </section>
              </div>
            </>
          ) : (
            <AccountChooser
              description={session.reauthenticationRequired
                ? "Your session expired. Enter your phone number to restore this account’s memory and connections."
                : "Enter your phone number to create or restore your Nanocodex account."}
              disabled={session.operation !== null}
              failure={session.error}
              onChooseAccount={(selection) => void session.chooseAccount(selection)}
            />
          )}
        </section>
      ) : null}
    </div>
  );
}

function TempoWalletConnectionCard({
  address,
  balance,
  fundingAmountCents,
  fundingAvailable,
  fundingError,
  fundingOperation,
  fundingSuccess,
  onFund,
}: Readonly<{
  address?: string | undefined;
  balance: string;
  fundingAmountCents: number;
  fundingAvailable: boolean;
  fundingError: string | null;
  fundingOperation: "prepare" | "payment" | null;
  fundingSuccess: string | null;
  onFund(): void;
}>) {
  const busy = fundingOperation !== null;
  return (
    <div className="wizard-connector-card tempo-wallet-connection" id="wallet" role="listitem">
      <div className={`connection-card tempo-wallet-card${address ? " is-connected" : " is-unavailable"}`}>
        <ConnectionLogo id="tempo" />
        <span className="connection-card-copy">
          <strong>{busy ? "Add funds" : "Wallet"}</strong>
          <span className="tempo-wallet-balance" role={busy ? "status" : undefined}>{fundingOperation === "prepare"
            ? "Preparing secure checkout…"
            : fundingOperation === "payment"
              ? "Complete payment in Stripe"
              : balance}</span>
          {fundingError ? <span className="tempo-wallet-message is-error" role="alert">{fundingError}</span> : null}
          {fundingSuccess ? <span className="tempo-wallet-message is-success" role="status">{fundingSuccess}</span> : null}
        </span>
        {busy ? <span className="tempo-wallet-payment-status" role="status">Waiting</span> : (
          <span className="tempo-wallet-card-actions">
            <button disabled={!address || !fundingAvailable} onClick={onFund} type="button">
              {fundingAvailable ? `Add ${formatDollars(fundingAmountCents)}` : "Onramp unavailable"}
            </button>
          </span>
        )}
      </div>
    </div>
  );
}

async function apiRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

function decodeApiKey(value: unknown): ApiKeyMetadata {
  if (!isRecord(value)) throw new Error("Invalid API key response.");
  const { id, label, prefix, createdAt } = value;
  if (
    typeof id !== "string"
    || !API_KEY_ID.test(id)
    || typeof label !== "string"
    || typeof prefix !== "string"
    || typeof createdAt !== "number"
    || !Number.isFinite(createdAt)
  ) throw new Error("Invalid API key response.");
  return { id, label, prefix, createdAt };
}

function decodeCredentialStatus(value: unknown): CredentialStatus {
  if (!isRecord(value) || !isRecord(value.openai) || !isRecord(value.chatgpt)) {
    throw new Error("Invalid model connection response.");
  }
  const active = value.active === "openai" || value.active === "chatgpt" ? value.active : null;
  if (typeof value.ready !== "boolean"
    || typeof value.openai.connected !== "boolean"
    || typeof value.chatgpt.connected !== "boolean") {
    throw new Error("Invalid model connection response.");
  }
  const login = value.chatgpt.login === undefined
    ? undefined
    : decodeChatGptLogin(value.chatgpt.login);
  return {
    ready: value.ready,
    active,
    openai: { connected: value.openai.connected },
    chatgpt: {
      connected: value.chatgpt.connected,
      ...(typeof value.chatgpt.account_id === "string" ? { accountId: value.chatgpt.account_id } : {}),
      ...(login ? { login } : {}),
    },
  };
}

function decodeChatGptLogin(value: unknown): NonNullable<CredentialStatus["chatgpt"]["login"]> {
  if (!isRecord(value)
    || value.state !== "pending"
    || typeof value.verification_url !== "string"
    || typeof value.user_code !== "string"
    || typeof value.expires_at !== "number"
    || typeof value.poll_after_ms !== "number") {
    throw new Error("Invalid ChatGPT sign-in response.");
  }
  return {
    verificationUrl: value.verification_url,
    userCode: value.user_code,
    expiresAt: value.expires_at,
    pollAfterMs: value.poll_after_ms,
  };
}

function shortIdentity(id: string): string {
  return `${id.slice(0, 8)}…${id.slice(-4)}`;
}

function failureMessage(cause: unknown, fallback: string): string {
  return clientFailureMessage(cause, fallback);
}

function notifyModelCredentialChanged(): void {
  deploymentHealth.invalidate();
  window.dispatchEvent(new Event("nanocodex:model-credential-changed"));
}
