import {
  lazy,
  memo,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { AgentControllerEvent } from "nanocodex-react/agent";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { Link, useNavigate } from "react-router";
import { Moon, PanelLeft, SquarePen, Sun } from "lucide-react";
import type { AgentStatus, AgentTerminalMode, AgentTerminalState } from "./agentTerminalTypes";
import { AgentTerminal, ManagedAgentTerminal } from "./AgentTerminal";
import { TerminalTranscriptSurface } from "nanocodex-terminal";
import { AgentSidebar } from "./AgentSidebar";
import { useAccountSession } from "./AccountSession";
import { browserAgentCapabilityError } from "./browserAgentCapabilities";
import { clientFailureMessage } from "./clientFailure";
import {
  inactiveTerminalMessage,
  useModelSession,
  type ModelSessionStatus,
  type CredentialSource,
} from "./modelSession";
import { conversationTitle } from "./localConversationRuntime";
import {
  createManagedConversation,
  loadManagedConversationSelection,
  type ManagedConversation,
} from "./managedAgentRuntime";
import "nanocodex-connect-ui/styles.css";
import "./AgentTerminal.css";
import "nanocodex-terminal/styles.css";
import "./Home.css";
import { formatDollars } from "./walletFunding";
import { useWalletFunding } from "./useWalletFunding";
import { homeTerminalWelcome } from "./homeTerminalWelcome";

/** Ephemeral homepage consumer and managed-durable Agent demo. */
export const AgentExperience = memo(function AgentExperience({
  agentId,
  landing,
  mode,
  onAgentChange,
  theme,
  onThemeChange,
}: {
  agentId?: string;
  landing?: boolean;
  mode: AgentTerminalMode;
  onAgentChange?(agentId: string, options?: { replace?: boolean }): void;
  theme: "light" | "dark";
  onThemeChange(theme: "light" | "dark"): void;
}) {
  const navigate = useNavigate();
  const [ephemeralThreadId, setEphemeralThreadId] = useState(() => crypto.randomUUID());
  const account = useAccountSession();
  const capabilityError = useMemo(() => browserAgentCapabilityError(), []);
  const [authStatus, setAuthStatus] = useState<ModelSessionStatus>();
  const [credentialSource, setCredentialSource] = useState<CredentialSource>();
  const credentialSourceRef = useRef<CredentialSource | undefined>(undefined);
  const [runtimeState, setRuntimeState] = useState<AgentTerminalState>();
  const [railOpen, setRailOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => safeGet("nanocodex:sidebar-collapsed") === "true");
  const toggleDesktopSidebar = () => setSidebarCollapsed((collapsed) => { safeSet("nanocodex:sidebar-collapsed", String(!collapsed)); return !collapsed; });
  const sidebarTriggerRef = useRef<HTMLButtonElement>(null);
  const closeSidebar = useCallback(() => setRailOpen(false), []);
  const [managedConversations, setManagedConversations] = useState<readonly ManagedConversation[]>([]);
  const [managedConversationId, setManagedConversationId] = useState<string>();
  const [managedError, setManagedError] = useState<string>();
  const [managedAttempt, setManagedAttempt] = useState(0);
  const refreshManagedList = useRef(false);
  const [conversationPending, setConversationPending] = useState(false);
  const [freePromptsRemaining, setFreePromptsRemaining] = useState<number | null>(null);
  const [sponsoredExhausted, setSponsoredExhausted] = useState(false);
  const hasCredential = credentialSource === "brokered" || credentialSource === "sponsored";
  const hasDurableCredential = credentialSource === "brokered";
  const showHomepageSms = landing
    && account.status !== "checking"
    && account.account?.persistent !== true;
  const showHomepageTrialActions = landing
    && credentialSource === "sponsored"
    && sponsoredExhausted;
  const showHomepageTrialReset = landing && credentialSource === "sponsored";
  const activeCapabilityError = landing ? capabilityError : undefined;
  const canRun = landing
    ? hasCredential && !(credentialSource === "sponsored" && sponsoredExhausted)
    : hasDurableCredential;
  const showHomepageTerminal = landing && hasCredential && !activeCapabilityError;
  const voiceEnabled = authStatus?.state === "ready" && authStatus.voiceEnabled === true;
  const visibleManagedConversationId = agentId === undefined || managedConversationId === agentId
    ? managedConversationId
    : undefined;

  useEffect(() => {
    setManagedConversations([]);
    setManagedConversationId(undefined);
    setRuntimeState(undefined);
  }, [account.account?.id]);
  useEffect(() => {
    const remaining = authStatus?.state === "ready" && credentialSource === "sponsored"
      ? authStatus.freePromptsRemaining
      : null;
    setFreePromptsRemaining(remaining);
    setSponsoredExhausted(remaining === 0);
  }, [account.account?.id, authStatus, credentialSource]);
  useEffect(() => {
    if (landing || account.status !== "ready" || !account.account || !hasDurableCredential
      || authStatus?.state !== "ready") return;
    let cancelled = false;
    const accountId = account.account.id;
    const refresh = refreshManagedList.current;
    refreshManagedList.current = false;
    if (agentId) {
      setManagedConversationId((current) => current === agentId ? current : undefined);
      setRuntimeState(undefined);
    }
    setConversationPending(true);
    setManagedError(undefined);
    void loadManagedConversationSelection({
      accountId,
      routeAgentId: agentId,
      retainedAgentId: safeGet(managedSelectionKey(accountId)) ?? undefined,
      hasCredential,
      refresh,
    }).then((selection) => {
      if (cancelled) return;
      setManagedConversations(selection.conversations);
      setManagedConversationId(selection.selectedId);
      if (selection.selectedId) {
        safeSet(managedSelectionKey(accountId), selection.selectedId);
        if (selection.replaceRoute) onAgentChange?.(selection.selectedId, { replace: true });
      }
    }).catch((error) => {
      if (!cancelled) setManagedError(errorMessage(error));
    }).finally(() => {
      if (!cancelled) setConversationPending(false);
    });
    return () => { cancelled = true; };
  }, [
    account.account?.id,
    account.status,
    authStatus,
    agentId,
    hasDurableCredential,
    landing,
    managedAttempt,
    onAgentChange,
  ]);

  const changeCredentialSource = useCallback((source: CredentialSource) => {
    if (credentialSourceRef.current !== undefined && credentialSourceRef.current !== source) {
      setRuntimeState(undefined);
    }
    credentialSourceRef.current = source;
    setCredentialSource(source);
  }, []);
  const { retrySession: refreshModelSession } = useModelSession({
    onStatusChange: setAuthStatus,
    onSourceChange: changeCredentialSource,
  });
  const effectiveAuthStatus = useMemo(
    () => authStatus?.state === "ready" && credentialSource === "sponsored"
      ? { ...authStatus, freePromptsRemaining }
      : authStatus,
    [authStatus, credentialSource, freePromptsRemaining],
  );
  const sessionChecking = account.status === "checking" || authStatus === undefined || credentialSource === undefined;
  const agentStatus: AgentStatus = sessionChecking ? "starting" : !canRun || activeCapabilityError
    ? "idle" : runtimeState?.status ?? "starting";
  const agentError = runtimeState?.error;
  const inactiveMessage = inactiveTerminalMessage({
    agentError, agentStatus, authStatus: effectiveAuthStatus, capabilityError: activeCapabilityError,
    runtime: landing ? "browser" : "managed", source: credentialSource,
  });

  const selectManaged = useCallback((id: string) => {
    setRailOpen(false);
    if (id === visibleManagedConversationId) return;
    setManagedConversationId(id);
    if (account.account) safeSet(managedSelectionKey(account.account.id), id);
    setRuntimeState(undefined);
    onAgentChange?.(id);
  }, [account.account, onAgentChange, visibleManagedConversationId]);
  const createConversation = useCallback(() => {
    if (conversationPending || !account.account) return;
    setConversationPending(true);
    setManagedError(undefined);
    void createManagedConversation(account.account.id).then((conversation) => {
      setManagedConversations((current) => [conversation, ...current]);
      setManagedConversationId(conversation.id);
      safeSet(managedSelectionKey(account.account!.id), conversation.id);
      setRuntimeState(undefined);
      setRailOpen(false);
      onAgentChange?.(conversation.id);
    }).catch((error) => setManagedError(errorMessage(error)))
      .finally(() => setConversationPending(false));
  }, [account.account, conversationPending, onAgentChange]);
  const retryManagedConversations = useCallback(() => {
    setManagedError(undefined);
    refreshManagedList.current = true;
    setManagedAttempt((value) => value + 1);
  }, []);
  const recordActivity = useCallback((input: string) => {
    if (!managedConversationId) return;
    setManagedConversations((current) => current.map((item) => item.id === managedConversationId ? {
      ...item,
      title: (item.turnCount ?? 0) === 0 ? conversationTitle(input) : item.title,
      turnCount: (item.turnCount ?? 0) + 1,
      updatedAt: Date.now(),
    } : item).sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0)));
  }, [managedConversationId]);
  const recordSponsoredActivity = useCallback(() => {
    // The egress reservation, not local submission, owns the allowance.
  }, []);
  const observeSponsoredTerminal = useCallback((event: AgentControllerEvent) => {
    if (credentialSource !== "sponsored"
      || (event.type !== "prompt.completed" && event.type !== "prompt.failed")) return;
    void refreshModelSession();
  }, [credentialSource, refreshModelSession]);
  const acceptSponsoredTrialReset = useCallback(async () => {
    setFreePromptsRemaining(3);
    setSponsoredExhausted(false);
    setRuntimeState(undefined);
    setEphemeralThreadId(crypto.randomUUID());
    await refreshModelSession();
  }, [refreshModelSession]);

  const newChat = () => {
    closeSidebar();
    if (landing) {
      setRuntimeState(undefined);
      setEphemeralThreadId(crypto.randomUUID());
    } else if (hasDurableCredential) createConversation();
    else void navigate("/connect");
  };
  const selectedConversation = managedConversations.find(({ id }) => id === visibleManagedConversationId);
  const title = landing ? "New chat" : selectedConversation
    ? /^Conversation [a-f\d]{8}$/i.test(selectedConversation.title) ? "New agent" : selectedConversation.title
    : "Your agents";

  return <div className={`nanocodex-demo chat-workspace is-${mode}${landing ? " is-landing" : ""}`}>
    <div className={`conversation-workspace${sidebarCollapsed ? " is-sidebar-collapsed" : ""}`}>
      <AgentSidebar key={account.account?.id ?? "anonymous"}
        conversations={managedConversations} error={managedError} landing={!!landing} active={mode !== "hidden"}
        open={railOpen && mode !== "hidden"} pending={conversationPending} selectedId={visibleManagedConversationId}
        onClose={closeSidebar} onCollapse={toggleDesktopSidebar} collapsed={sidebarCollapsed} onCreate={newChat} onRetry={retryManagedConversations} onSelect={selectManaged}
        persistent={account.account?.persistent === true} triggerRef={sidebarTriggerRef}
      />
      <div className="conversation-main">
        <header className="agent-chat-header">
          <button ref={sidebarTriggerRef} className="agent-sidebar-toggle chat-icon-button" type="button" onClick={() => { if (window.matchMedia("(min-width: 761px)").matches) toggleDesktopSidebar(); else setRailOpen(true); }} aria-label="Open sidebar" aria-expanded={railOpen} aria-controls="agent-navigation"><PanelLeft aria-hidden="true" /></button>
          <div className="agent-chat-heading"><strong>{landing ? "Nanocodex" : title}</strong></div>
          <div className="agent-chat-header-actions">
            {agentStatus === "starting" || agentStatus === "error" ? <span className={`agent-chat-status is-${agentStatus}`} role="status"><i aria-hidden="true" />{agentStatus === "starting" ? "Connecting…" : "Needs attention"}</span> : null}
            <button className="chat-icon-button" type="button" onClick={() => onThemeChange(theme === "light" ? "dark" : "light")} aria-label={`Use ${theme === "light" ? "dark" : "light"} appearance`} title={`Use ${theme === "light" ? "dark" : "light"} appearance`}>{theme === "light" ? <Moon aria-hidden="true" /> : <Sun aria-hidden="true" />}</button>
            <button className="chat-icon-button" type="button" disabled={conversationPending} onClick={newChat} aria-label={landing ? "New chat" : "New agent"} title={landing ? "New chat" : "New agent"}><SquarePen aria-hidden="true" /></button>
          </div>
        </header>
        {LOCAL_SPONSORED_TRIAL_RESET && showHomepageTrialReset ? (
          <Suspense fallback={null}>
            <LocalSponsoredTrialReset onReset={acceptSponsoredTrialReset} />
          </Suspense>
        ) : null}
        {showHomepageTerminal
          ? <AgentTerminal
            key={`ephemeral:${account.account?.id ?? "anonymous"}:${ephemeralThreadId}`}
            authStatus={effectiveAuthStatus}
            capabilityError={activeCapabilityError}
            composer={showHomepageTrialActions ? <HomepageTrialActions /> : undefined}
            enabled
            mode={mode} onConversationActivity={recordSponsoredActivity}
            onTerminalEvent={observeSponsoredTerminal}
            onStateChange={setRuntimeState} source={credentialSource} threadId={ephemeralThreadId}
            voiceEnabled={voiceEnabled}
            welcome={homeTerminalWelcome(credentialSource, freePromptsRemaining)}
          />
          : landing
            ? <ReservedTerminal
              composer={showHomepageSms
                ? <HomepageSmsTerminal />
                : showHomepageTrialActions ? <HomepageTrialActions /> : null}
              message={showHomepageSms || showHomepageTrialActions ? "" : inactiveMessage}
              mode={mode}
              welcome={homeTerminalWelcome(credentialSource, freePromptsRemaining)}
            />
            : managedError
              ? <ReservedTerminal message={managedError} mode={mode} />
              : hasDurableCredential && visibleManagedConversationId
                ? <ManagedAgentTerminal
                  key={visibleManagedConversationId} agentId={visibleManagedConversationId} authStatus={authStatus}
                  mode={mode} onConversationActivity={recordActivity} onStateChange={setRuntimeState}
                  source={credentialSource}
                  voiceEnabled={voiceEnabled}
                />
                : <ReservedTerminal message={inactiveMessage} mode={mode}
                  welcome={sessionChecking ? undefined : "# What should we work on?"}
                  composer={sessionChecking ? <p className="agent-connection-loading" role="status">Opening your workspace…</p>
                    : !hasDurableCredential ? <div className="agent-connect-prompt"><Link to="/connect">Connect your account</Link><span>Connect a model account to start a durable agent.</span></div> : null}
                />}
        <p className="agent-chat-footnote">{landing ? "Chats here are temporary. Use Agents to keep your work across sessions." : "Your agent keeps working when you leave. Come back anytime."}</p>
      </div>
    </div>
  </div>;
});

function ReservedTerminal({
  composer = null,
  message,
  mode,
  welcome,
}: {
  composer?: ReactNode;
  message: string;
  mode: AgentTerminalMode;
  welcome?: string;
}) {
  return <TerminalTranscriptSurface
    canLoadOlder={false}
    composer={composer}
    entries={[]}
    inactiveMessage={message}
    isLoadingOlder={false}
    mode={mode}
    status="idle"
    welcome={welcome}
    onLoadOlder={NO_OLDER_HISTORY}
  />;
}

function HomepageSmsTerminal() {
  const account = useAccountSession();
  return <div className="connect-onboarding terminal-sms-auth">
    <AccountChooser
      description={account.reauthenticationRequired
        ? "Your session expired. Enter your phone number to restore it and unlock your free prompts."
        : "Verify by SMS to unlock three free Luna prompts. No ChatGPT connection is required."}
      disabled={account.operation !== null}
      failure={account.error}
      onChooseAccount={(selection) => void account.chooseAccount(selection)}
    />
  </div>;
}

function HomepageTrialActions() {
  const funding = useWalletFunding(true);
  return <div className="homepage-trial-actions">
    <div>
      <strong>Your three free prompts are used.</strong>
      <span>{funding.error ?? "Connect your own model account for durable agents, or add funds to your Wallet."}</span>
    </div>
    <nav aria-label="Continue after free prompts">
      <Link to="/connect">Connect</Link>
      <button
        disabled={funding.loading || !funding.available || funding.operation !== null}
        onClick={funding.fund}
        type="button"
      >
        {funding.operation === "prepare"
          ? "Preparing checkout…"
          : funding.operation === "payment"
            ? "Opening Stripe…"
            : funding.loading
              ? "Loading Wallet…"
              : `Fund Wallet · ${formatDollars(funding.amountCents)}`}
      </button>
    </nav>
  </div>;
}

const NO_OLDER_HISTORY = async () => false;
const LOCAL_SPONSORED_TRIAL_RESET = typeof __NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__ !== "undefined"
  && __NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__;
const LocalSponsoredTrialReset = LOCAL_SPONSORED_TRIAL_RESET
  ? lazy(async () => ({
    default: (await import("./LocalSponsoredTrialReset")).LocalSponsoredTrialReset,
  }))
  : () => null;

function managedSelectionKey(accountId: string) {
  return `nanocodex.managed-conversation.v2.${accountId}`;
}
function safeGet(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}
function safeSet(key: string, value: string) {
  try { localStorage.setItem(key, value); } catch {}
}
function errorMessage(error: unknown) {
  return clientFailureMessage(
    error,
    "Managed agents could not be reached. Check your network and retry.",
  );
}
